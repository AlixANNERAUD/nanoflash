//! The native transport over a pair of pseudo-terminals linked by `socat`.
//!
//! Without `socat` the tests pass early, unless `NANOFLASH_REQUIRE_SOCAT=1` makes that a failure.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nanoflash::native::{NativePortProvider, NativeSerialPort};
use nanoflash::transport::{DeviceFilter, PortProvider, SerialPort, TransportError};

struct Pair {
    child: Child,
    directory: PathBuf,
}

impl Pair {
    fn a(&self) -> String {
        self.directory.join("a").to_string_lossy().into_owned()
    }

    fn b(&self) -> String {
        self.directory.join("b").to_string_lossy().into_owned()
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Starts `socat`, or returns `None` when it is missing and not required.
fn start(name: &str) -> Option<Pair> {
    let required = std::env::var("NANOFLASH_REQUIRE_SOCAT").is_ok_and(|value| value == "1");
    let directory =
        std::env::temp_dir().join(format!("nanoflash-pty-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a temporary directory");
    let spawned = Command::new("socat")
        .arg("-d")
        .arg("-d")
        .arg(format!(
            "pty,raw,echo=0,link={}",
            directory.join("a").display()
        ))
        .arg(format!(
            "pty,raw,echo=0,link={}",
            directory.join("b").display()
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let child = match spawned {
        Ok(child) => child,
        Err(error) => {
            assert!(!required, "socat is required but cannot start: {error}");
            eprintln!("skipped: socat is not available ({error})");
            return None;
        }
    };
    let pair = Pair { child, directory };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !(pair.directory.join("a").exists() && pair.directory.join("b").exists()) {
        assert!(Instant::now() < deadline, "socat did not create both links");
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(pair)
}

#[test]
fn pty_round_trip() {
    let Some(pair) = start("round-trip") else {
        return;
    };
    pollster::block_on(async {
        let mut a = NativeSerialPort::open(&pair.a(), 115200).expect("open a");
        let mut b = NativeSerialPort::open(&pair.b(), 115200).expect("open b");
        a.write(&[1, 2, 3]).await.expect("write");
        let mut buffer = [0u8; 8];
        let mut received = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while received < 3 && Instant::now() < deadline {
            received += b
                .read(&mut buffer[received..], Duration::from_millis(100))
                .await
                .expect("read");
        }
        assert_eq!(&buffer[..received], &[1, 2, 3]);
        assert_eq!(b.read(&mut buffer, Duration::from_millis(50)).await, Ok(0));
        assert_eq!(b.set_baud_rate(460800).await, Ok(()));
        assert_eq!(a.identity(), None);
    });
}

#[test]
fn provider_gives_up_with_not_found() {
    let Some(_pair) = start("not-found") else {
        return;
    };
    let filter = DeviceFilter {
        vendor: 0xffff,
        product: Some(0xffff),
        serial_number: None,
    };
    let result =
        pollster::block_on(NativePortProvider.open(&filter, 115200, Duration::from_millis(300)));
    assert_eq!(
        result.err(),
        Some(TransportError::NotFound {
            filter: "ffff:ffff".into()
        })
    );
}
