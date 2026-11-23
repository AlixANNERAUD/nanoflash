//! Flashes the boards attached to this machine. Ignored unless asked for:
//! `NANOFLASH_HARDWARE=hardware/boards.toml cargo test -p nanoflash-cli --test hardware -- --ignored`.

use nanoflash::native::NativeSerialPort;
use nanoflash::transport::SerialPort;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// How long the firmware has to print its expected text after a flash.
const BANNER_WAIT: Duration = Duration::from_secs(15);

struct Board {
    name: String,
    manifest: PathBuf,
    port: String,
    expect: String,
}

fn text<'a>(table: &'a toml::Table, key: &str, file: &Path) -> &'a str {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "{}: every [[board]] needs a string \"{key}\"",
                file.display()
            )
        })
}

fn read_boards(file: &Path) -> Vec<Board> {
    let content = std::fs::read_to_string(file).unwrap_or_else(|error| {
        panic!(
            "NANOFLASH_HARDWARE names {}, which cannot be read: {error}",
            file.display()
        )
    });
    let document: toml::Table = content
        .parse()
        .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    let directory = file.parent().unwrap_or(Path::new("."));
    let boards = document
        .get("board")
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("{}: no [[board]] table", file.display()));
    boards
        .iter()
        .map(|board| {
            let table = board
                .as_table()
                .unwrap_or_else(|| panic!("{}: [[board]] must be a table", file.display()));
            Board {
                name: text(table, "name", file).to_string(),
                manifest: directory.join(text(table, "manifest", file)),
                port: text(table, "port", file).to_string(),
                expect: text(table, "expect", file).to_string(),
            }
        })
        .collect()
}

fn nanoflash(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nanoflash"))
        .args(arguments)
        .output()
        .expect("the nanoflash binary runs")
}

fn json_of(output: &Output, what: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{what} printed invalid JSON: {error}"))
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

/// Reads the port until `expect` appears or `BANNER_WAIT` passes.
fn wait_for_text(port: &str, expect: &str) -> Result<(), String> {
    let start = Instant::now();
    // The USB-Serial-JTAG port re-enumerates after the reset: retry the open.
    let mut serial = loop {
        match NativeSerialPort::open(port, 115_200) {
            Ok(serial) => break serial,
            Err(error) if start.elapsed() >= BANNER_WAIT => {
                return Err(format!("{port} could not be opened: {error}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    };
    let mut seen = Vec::new();
    let mut buffer = [0u8; 256];
    while start.elapsed() < BANNER_WAIT {
        let count = pollster::block_on(serial.read(&mut buffer, Duration::from_millis(500)))
            .map_err(|error| error.to_string())?;
        seen.extend_from_slice(buffer.get(..count).unwrap_or_default());
        if String::from_utf8_lossy(&seen).contains(expect) {
            return Ok(());
        }
    }
    Err(format!(
        "{port}: \"{expect}\" did not appear within {} s, the port printed {:?}",
        BANNER_WAIT.as_secs(),
        String::from_utf8_lossy(&seen)
    ))
}

/// The chip a manifest names, for the first board.
fn manifest_chip(manifest: &Path) -> String {
    let content = std::fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
    let document: toml::Table = content
        .parse()
        .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
    document
        .get("board")
        .and_then(toml::Value::as_array)
        .and_then(|boards| boards.first())
        .and_then(toml::Value::as_table)
        .map(|table| text(table, "chip", manifest).to_string())
        .unwrap_or_else(|| panic!("{}: no [[board]] table", manifest.display()))
}

#[test]
#[ignore = "needs a board; set NANOFLASH_HARDWARE"]
fn flashes_every_attached_board() {
    let Some(file) = std::env::var_os("NANOFLASH_HARDWARE") else {
        return;
    };
    // Tests run in the crate's directory: a relative path is relative to the repository root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file = root.join(file);
    let boards = read_boards(&file);
    assert!(!boards.is_empty(), "{} lists no board", file.display());
    let output_directory = root.join("target/hardware");
    std::fs::create_dir_all(&output_directory).expect("target/hardware can be created");

    for board in boards {
        let flash_transcript = output_directory.join(format!("{}-flash.transcript", board.name));
        let output = nanoflash(&[
            "flash",
            "--board",
            path_text(&board.manifest),
            "--port",
            &board.port,
            "--json",
            "--record",
            path_text(&flash_transcript),
        ]);
        let document = json_of(&output, &format!("flashing {}", board.name));
        assert_eq!(
            document["report"]["verified"], true,
            "{}: the flash was not verified: {document}",
            board.name
        );

        if let Err(message) = wait_for_text(&board.port, &board.expect) {
            panic!("{}: {message}", board.name);
        }

        let detect_transcript = output_directory.join(format!("{}-detect.transcript", board.name));
        let output = nanoflash(&[
            "detect",
            "--port",
            &board.port,
            "--json",
            "--record",
            path_text(&detect_transcript),
        ]);
        let document = json_of(&output, &format!("detecting {}", board.name));
        let chip = manifest_chip(&board.manifest);
        assert_eq!(
            document["chip"]["chip"].as_str(),
            Some(chip.as_str()),
            "{}: detect answered {document}",
            board.name
        );
    }
}
