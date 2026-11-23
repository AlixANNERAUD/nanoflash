//! Transcript tests of the Espressif backend.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::Path;
use std::rc::Rc;

use md5::{Digest, Md5};
use nanoflash::backend::{Backend, ChipInformation, ProgressEvent, Region, Segment, Stage};
use nanoflash::espressif::{Chip, EspressifBackend, EspressifOptions};
use nanoflash::manifest::{Protocol, ResetSequence};
use nanoflash::testing::{Event, HostToken, ReplayProvider, Transcript, record, replay};
use nanoflash::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};
use nanoflash::{Error, ErrorClass};
use pollster::block_on;

const MEBIBYTE: u64 = 1024 * 1024;

fn transcript(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/transcripts/espressif")
        .join(format!("{name}.transcript"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Connects over a replay of `text`; also returns the messages the backend reported.
fn run_text(
    text: &str,
    reset: Option<ResetSequence>,
    options: EspressifOptions,
) -> (
    Result<ChipInformation, Error>,
    ReplayProvider,
    Vec<String>,
    Option<Chip>,
) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let mut messages = Vec::new();
    let (result, chip) = {
        let mut backend = EspressifBackend::new(port, &mut provider, reset, options).unwrap();
        let mut progress = Messages(&mut messages);
        let result = block_on(backend.connect(&mut progress));
        (result, backend.chip())
    };
    (result, provider, messages, chip)
}

fn run(
    name: &str,
    reset: Option<ResetSequence>,
    options: EspressifOptions,
) -> (Result<ChipInformation, Error>, ReplayProvider) {
    let (result, provider, _, _) = run_text(&transcript(name), reset, options);
    (result, provider)
}

struct Messages<'a>(&'a mut Vec<String>);

impl nanoflash::backend::Progress for Messages<'_> {
    fn report(&mut self, _event: ProgressEvent) {}

    fn message(&mut self, text: &str) {
        self.0.push(text.to_string());
    }
}

fn usb_jtag() -> Option<UsbIdentity> {
    Some(UsbIdentity {
        vendor: 0x303a,
        product: 0x1001,
        serial_number: None,
    })
}

fn attempts(connect_attempts: u8, baud_rate: Option<u32>) -> EspressifOptions {
    EspressifOptions {
        baud_rate,
        connect_attempts,
    }
}

#[test]
fn connects_to_an_esp32_s3_over_usb() {
    let (result, provider, messages, chip) = run_text(
        &transcript("connect-esp32s3-usb"),
        None,
        EspressifOptions::default(),
    );
    assert_eq!(
        result.unwrap(),
        ChipInformation {
            protocol: Protocol::Espressif,
            chip: "esp32-s3".into(),
            flash_size: Some(8 * MEBIBYTE),
            secure_download_mode: false,
            usb: usb_jtag(),
        }
    );
    assert_eq!(chip, Some(Chip::Esp32S3));
    assert!(messages.is_empty(), "{messages:?}");
    provider.finish().unwrap();
}

#[test]
fn usb_serial_jtag_keeps_115200_baud() {
    let (result, provider, messages, _) = run_text(
        &transcript("connect-esp32s3-usb"),
        Some(ResetSequence::EspressifUsbSerialJtag),
        attempts(4, Some(921_600)),
    );
    result.unwrap();
    provider.finish().unwrap();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("keeping 115200 baud"), "{messages:?}");
}

#[test]
fn connects_to_an_esp32_through_a_bridge_on_the_second_reset() {
    let (result, provider) = run("connect-esp32-classic", None, attempts(2, Some(115_200)));
    let information = result.unwrap();
    assert_eq!(information.chip, "esp32");
    assert_eq!(information.flash_size, Some(4 * MEBIBYTE));
    assert_eq!(information.usb, None);
    assert!(!information.secure_download_mode);
    provider.finish().unwrap();
}

#[test]
fn reopens_a_port_that_left_during_the_reset() {
    let (result, provider) = run("connect-reopen", None, EspressifOptions::default());
    assert_eq!(result.unwrap().chip, "esp32-s3");
    provider.finish().unwrap();
}

#[test]
fn names_the_boot_mode_when_not_in_download_mode() {
    let (result, provider) = run("connect-wrong-boot-mode", None, attempts(1, None));
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::NotInDownloadMode {
            boot_mode: "0x8".into()
        }
    );
    assert_eq!(error.class(), ErrorClass::Device);
    assert!(error.to_string().contains("BOOT"), "{error}");
    provider.finish().unwrap();
}

#[test]
fn no_sync_after_every_reset() {
    let (result, provider) = run("connect-no-sync", None, attempts(1, None));
    let error = result.unwrap_err();
    assert_eq!(error, Error::NoSync { attempts: 1 });
    assert_eq!(error.class(), ErrorClass::Device);
    assert!(error.to_string().contains("after 1 resets"), "{error}");
    provider.finish().unwrap();
}

#[test]
fn refuses_the_esp8266() {
    let (result, provider) = run("connect-esp8266", None, EspressifOptions::default());
    assert_eq!(
        result.unwrap_err(),
        Error::UnsupportedChip {
            chip: "esp8266".into()
        }
    );
    provider.finish().unwrap();
}

#[test]
fn secure_download_mode_skips_register_access() {
    let (result, provider) = run("connect-secure-download", None, EspressifOptions::default());
    let information = result.unwrap();
    assert!(information.secure_download_mode);
    assert_eq!(information.flash_size, None);
    assert_eq!(information.chip, "esp32-s3");
    provider.finish().unwrap();
}

#[test]
fn changes_the_baud_rate_on_a_bridge() {
    for baud_rate in [Some(460_800), None] {
        let (result, provider) = run("connect-baud", None, attempts(2, baud_rate));
        assert_eq!(result.unwrap().chip, "esp32", "{baud_rate:?}");
        provider.finish().unwrap();
    }
}

#[test]
fn endless_noise_gives_no_response() {
    let full = transcript("connect-esp32s3-usb");
    let mut text = String::new();
    // The identity, the reset and the answered SYNC, up to the GET_SECURITY_INFO request.
    for line in full.lines() {
        text.push_str(line);
        text.push('\n');
        if line.starts_with("> c0 00 14") {
            break;
        }
    }
    let noise = vec!["55"; 1024].join(" ");
    for _ in 0..1100 {
        writeln!(text, "< {noise}").unwrap();
    }
    let (result, _, _, _) = run_text(&text, None, EspressifOptions::default());
    assert_eq!(
        result.unwrap_err(),
        Error::NoResponse {
            command: "GET_SECURITY_INFO"
        }
    );
}

#[test]
fn touch_resets_are_refused() {
    for (reset, name) in [
        (ResetSequence::Touch1200, "touch-1200"),
        (ResetSequence::Uf2Touch1200, "uf2-touch-1200"),
    ] {
        let (port, mut provider) = replay(Transcript::default());
        let error = EspressifBackend::new(port, &mut provider, Some(reset), Default::default())
            .err()
            .unwrap();
        assert_eq!(
            error,
            Error::UnsupportedReset {
                protocol: Protocol::Espressif,
                reset: name.into()
            }
        );
        assert_eq!(error.class(), ErrorClass::Input);
    }
}

#[test]
fn rom_refusals_name_the_command_and_the_code() {
    let error = Error::Rom {
        command: "SPI_ATTACH",
        code: 0x05,
    };
    assert_eq!(
        error.to_string(),
        "the ROM loader refused SPI_ATTACH: the message is malformed (0x05)"
    );
    assert_eq!(error.class(), ErrorClass::Device);
}

/// A port that answers every read with a full buffer of noise and counts what it sends.
struct EndlessNoise {
    /// The bytes read since each SYNC was written, one entry per SYNC.
    reads: Rc<RefCell<Vec<usize>>>,
}

impl SerialPort for EndlessNoise {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, TransportError> {
        buffer.fill(0x55);
        if let Some(count) = self.reads.borrow_mut().last_mut() {
            *count += buffer.len();
        }
        Ok(buffer.len())
    }

    async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
        self.reads.borrow_mut().push(0);
        Ok(())
    }

    async fn set_baud_rate(&mut self, _baud_rate: u32) -> Result<(), TransportError> {
        Ok(())
    }

    async fn set_signals(&mut self, _signals: Signals) -> Result<(), TransportError> {
        Ok(())
    }

    async fn sleep(&mut self, _duration: Duration) {}

    fn identity(&self) -> Option<UsbIdentity> {
        None
    }
}

struct NoProvider;

impl PortProvider for NoProvider {
    type Port = EndlessNoise;

    async fn open(
        &mut self,
        _filter: &DeviceFilter,
        _baud_rate: u32,
        _timeout: Duration,
    ) -> Result<EndlessNoise, TransportError> {
        Err(TransportError::Disconnected)
    }
}

#[test]
fn each_sync_try_gives_up_after_four_kibibytes_of_noise() {
    let reads = Rc::new(RefCell::new(Vec::new()));
    let port = EndlessNoise {
        reads: Rc::clone(&reads),
    };
    let mut provider = NoProvider;
    let mut backend = EspressifBackend::new(port, &mut provider, None, attempts(2, None)).unwrap();
    assert_eq!(
        block_on(backend.connect(&mut ())).unwrap_err(),
        Error::NoSync { attempts: 2 }
    );
    // Five SYNC tries after each of the two resets, each reading 4 KiB before the next one.
    assert_eq!(*reads.borrow(), vec![4096; 10]);
}

fn image(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/images")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The connection to the ESP32-S3 over USB followed by `name`.
fn after_connect(name: &str) -> String {
    transcript("connect-esp32s3-usb") + &transcript(name)
}

/// Connects through the `Backend` interface over a replay of `text`, then runs `then`.
fn session<T>(
    text: &str,
    reset: Option<ResetSequence>,
    then: impl FnOnce(&mut EspressifBackend<'_, ReplayProvider>) -> T,
) -> (T, ReplayProvider) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let result = {
        let mut backend =
            EspressifBackend::new(port, &mut provider, reset, EspressifOptions::default()).unwrap();
        assert_eq!(
            block_on(Backend::connect(&mut backend)).unwrap().chip,
            "esp32-s3"
        );
        then(&mut backend)
    };
    (result, provider)
}

fn hexadecimal(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The `<` line of an `SPI_FLASH_MD5` answer carrying `digits`.
fn md5_answer(digits: &str) -> String {
    let mut line = String::from("< c0 01 13 24 00 00 00 00 00");
    for byte in digits.bytes() {
        write!(line, " {byte:02x}").unwrap();
    }
    line.push_str(" 00 00 00 00 c0\n");
    line
}

/// Undoes the SLIP framing of one frame.
fn slip_decode(frame: &[u8]) -> Vec<u8> {
    let inner = &frame[1..frame.len() - 1];
    let mut out = Vec::new();
    let mut escaped = false;
    for &byte in inner {
        match (escaped, byte) {
            (false, 0xDB) => escaped = true,
            (false, _) => out.push(byte),
            (true, 0xDC) => {
                out.push(0xC0);
                escaped = false;
            }
            (true, 0xDD) => {
                out.push(0xDB);
                escaped = false;
            }
            (true, _) => panic!("bad escape {byte:02x}"),
        }
    }
    out
}

#[test]
fn writes_and_verifies_two_images() {
    let bootloader = image("bootloader.bin");
    let application = image("application.bin");
    let segments = [
        Segment {
            label: "bootloader",
            offset: 0,
            data: &bootloader,
        },
        Segment {
            label: "application",
            offset: 0x1_0000,
            data: &application,
        },
    ];
    let mut events = Vec::new();
    let (result, provider) = session(
        &after_connect("write-esp32s3"),
        None,
        |backend| -> Result<(), Error> {
            let mut progress = |event: ProgressEvent| events.push(event);
            block_on(backend.write(&segments, &mut progress))?;
            block_on(backend.verify(&segments, &mut progress))?;
            block_on(backend.reset_to_application())
        },
    );
    result.unwrap();
    provider.finish().unwrap();
    let writing: Vec<_> = events
        .iter()
        .filter(|event| event.stage == Stage::Writing)
        .collect();
    for (offset, size) in [(0, 0x1000), (0x1_0000, 0x2000)] {
        let last = writing
            .iter()
            .rfind(|event| event.offset == offset)
            .unwrap();
        assert_eq!((last.done, last.total), (size, size), "{events:?}");
    }
    let first_verifying = events
        .iter()
        .position(|event| event.stage == Stage::Verifying)
        .unwrap();
    assert!(
        events[first_verifying..]
            .iter()
            .all(|event| event.stage == Stage::Verifying),
        "{events:?}"
    );
    for offset in [0, 0x1_0000] {
        assert!(
            events[first_verifying..]
                .iter()
                .any(|event| event.offset == offset && event.done == event.total),
            "{events:?}"
        );
    }
}

/// `size` bytes that do not compress: a xorshift sequence.
fn incompressible(size: usize) -> Vec<u8> {
    let mut state: u32 = 0x1234_5678;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn compressed_blocks_inflate_to_the_image() {
    let bootloader = image("bootloader.bin");
    let application = image("application.bin");
    let noise = incompressible(0x800);
    let segments = [
        Segment {
            label: "bootloader",
            offset: 0,
            data: &bootloader,
        },
        Segment {
            label: "application",
            offset: 0x1_0000,
            data: &application,
        },
        Segment {
            label: "noise",
            offset: 0x2_0000,
            data: &noise,
        },
    ];
    // The writes of `write-esp32s3`, then the noise: 0x1000 bytes erased at 0x20000, in as many
    // blocks as its zlib stream needs.
    let mut text = transcript("connect-esp32s3-usb");
    for line in transcript("write-esp32s3").lines() {
        if line.starts_with("> c0 00 13") {
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    let blocks = miniz_oxide::deflate::compress_to_vec_zlib(&noise, 9)
        .len()
        .div_ceil(0x400);
    assert!(blocks >= 2, "{blocks}");
    writeln!(
        text,
        "> c0 00 10 14 00 00 00 00 00 00 10 00 00 {blocks:02x} 00 00 00 00 04 00 00 00 00 02 00 00 00 00 00 c0\n\
         < c0 01 10 04 00 00 00 00 00 00 00 00 00 c0"
    )
    .unwrap();
    for _ in 0..blocks {
        text.push_str("> c0 00 11 .. c0\n< c0 01 11 04 00 00 00 00 00 00 00 00 00 c0\n");
    }
    let (port, provider) = replay(Transcript::parse(&text).unwrap());
    let (port, mut provider) = record(port, provider);
    {
        let mut backend =
            EspressifBackend::new(port, &mut provider, None, EspressifOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        block_on(backend.write(&segments, &mut ())).unwrap();
    }
    provider.inner().finish().unwrap();
    // The data of the blocks of each FLASH_DEFL_BEGIN, in order, and the number of blocks.
    let mut streams: Vec<(Vec<u8>, u32)> = Vec::new();
    for entry in provider.transcript().events {
        let Event::Host(tokens) = entry.event else {
            continue;
        };
        let frame: Vec<u8> = tokens
            .iter()
            .map(|token| match token {
                HostToken::Byte(byte) => *byte,
                HostToken::Any => panic!("a recording has no wildcard"),
            })
            .collect();
        let packet = slip_decode(&frame);
        let word = |start: usize| u32::from_le_bytes(packet[start..start + 4].try_into().unwrap());
        match packet[1] {
            0x10 => streams.push((Vec::new(), 0)),
            0x11 => {
                let (stream, count) = streams.last_mut().unwrap();
                let length = word(8) as usize;
                let data = &packet[24..];
                assert_eq!(data.len(), length);
                let checksum = data.iter().fold(0xEF_u8, |sum, byte| sum ^ byte);
                assert_eq!(word(4), u32::from(checksum), "checksum of block {count}");
                assert_eq!(word(12), *count, "sequence");
                *count += 1;
                stream.extend_from_slice(data);
            }
            _ => {}
        }
    }
    assert_eq!(streams.len(), 3);
    assert_eq!(streams[2].1 as usize, blocks);
    for ((stream, _), expected) in streams.iter().zip([&bootloader, &application, &noise]) {
        assert_eq!(
            &miniz_oxide::inflate::decompress_to_vec_zlib(stream).unwrap(),
            expected
        );
    }
}

#[test]
fn pads_to_four_bytes() {
    let data = b"hello";
    let mut padded = data.to_vec();
    padded.extend_from_slice(&[0xFF; 3]);
    let digest = hexadecimal(&Md5::digest(&padded));
    let mut text = transcript("connect-esp32s3-usb");
    // 0x1000 bytes erased, one block, at 0x0.
    text.push_str(
        "> c0 00 10 14 00 00 00 00 00 00 10 00 00 01 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 c0\n\
         < c0 01 10 04 00 00 00 00 00 00 00 00 00 c0\n\
         > c0 00 11 .. c0\n\
         < c0 01 11 04 00 00 00 00 00 00 00 00 00 c0\n\
         > c0 00 13 10 00 00 00 00 00 00 00 00 00 08 00 00 00 00 00 00 00 00 00 00 00 c0\n",
    );
    text.push_str(&md5_answer(&digest));
    let segments = [Segment {
        label: "hello",
        offset: 0,
        data,
    }];
    let (result, provider) = session(&text, None, |backend| -> Result<(), Error> {
        block_on(backend.write(&segments, &mut ()))?;
        block_on(backend.verify(&segments, &mut ()))
    });
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn erases_a_region() {
    let (result, provider) = session(&after_connect("erase-esp32s3"), None, |backend| {
        block_on(backend.erase(Region {
            offset: 0,
            size: 0x80_0000,
        }))
    });
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn md5_mismatch_is_a_verification_failure() {
    let bootloader = image("bootloader.bin");
    let segments = [Segment {
        label: "bootloader",
        offset: 0,
        data: &bootloader,
    }];
    let (result, provider) = session(
        &after_connect("md5-mismatch"),
        None,
        |backend| -> Result<(), Error> {
            block_on(backend.write(&segments, &mut ()))?;
            block_on(backend.verify(&segments, &mut ()))
        },
    );
    provider.finish().unwrap();
    let error = result.unwrap_err();
    let expected = "a0c16616c91907bd14e999986cf822d5";
    let actual = "0".repeat(32);
    assert_eq!(
        error,
        Error::VerificationFailed {
            label: "bootloader".into(),
            offset: 0,
            size: 0x1000,
            expected: expected.into(),
            actual: actual.clone(),
        }
    );
    assert_eq!(error.class(), ErrorClass::Verification);
    let message = error.to_string();
    assert!(message.contains(expected), "{message}");
    assert!(message.contains(&actual), "{message}");
    assert!(message.contains("0x0"), "{message}");
    assert!(message.contains("\"bootloader\""), "{message}");
}

#[test]
fn rom_errors_name_the_command() {
    let bootloader = image("bootloader.bin");
    let segments = [Segment {
        label: "bootloader",
        offset: 0,
        data: &bootloader,
    }];
    let (result, provider) = session(&after_connect("rom-error"), None, |backend| {
        block_on(backend.write(&segments, &mut ()))
    });
    provider.finish().unwrap();
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::Rom {
            command: "FLASH_DEFL_DATA",
            code: 7
        }
    );
    assert!(error.to_string().contains("bad checksum"), "{error}");
}

#[test]
fn soft_reset_without_lines() {
    let (result, provider) = session(
        &transcript("reset-none"),
        Some(ResetSequence::None),
        |backend| block_on(backend.reset_to_application()),
    );
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn offsets_beyond_four_gibibytes_are_refused() {
    let data = [0_u8; 16];
    let segments = [
        Segment {
            label: "bootloader",
            offset: 0,
            data: &data,
        },
        Segment {
            label: "application",
            offset: 1 << 32,
            data: &data,
        },
    ];
    let (result, provider) = session(&transcript("connect-esp32s3-usb"), None, |backend| {
        (
            block_on(backend.write(&segments, &mut ())),
            block_on(backend.verify(&segments, &mut ())),
            block_on(backend.erase(Region {
                offset: 1 << 32,
                size: 0x1000,
            })),
        )
    });
    // Nothing was sent: the transcript ends with the connection.
    provider.finish().unwrap();
    let refused = Error::OutOfRange {
        what: "application offset".into(),
        value: 1 << 32,
    };
    assert_eq!(result.0, Err(refused.clone()));
    assert_eq!(result.1, Err(refused.clone()));
    assert!(matches!(result.2, Err(Error::OutOfRange { value, .. }) if value == 1 << 32));
    assert_eq!(refused.class(), ErrorClass::Input);
}

/// Two boards of the same model, told apart by their serial number: opens the first port the
/// filter selects, in order.
struct TwoBoards {
    ports: Vec<nanoflash::testing::ReplayPort>,
}

impl PortProvider for TwoBoards {
    type Port = nanoflash::testing::ReplayPort;

    async fn open(
        &mut self,
        filter: &DeviceFilter,
        _baud_rate: u32,
        _timeout: Duration,
    ) -> Result<Self::Port, TransportError> {
        let index = self
            .ports
            .iter()
            .position(|port| {
                port.identity()
                    .is_some_and(|identity| filter.matches(&identity))
            })
            .ok_or(TransportError::NotFound {
                filter: filter.to_string(),
            })?;
        Ok(self.ports.remove(index))
    }
}

#[test]
fn reopening_picks_the_board_with_the_same_serial_number() {
    let full = transcript("connect-reopen");
    let (before, after) = full.split_once("! reopen 303a:1001\n").unwrap();
    // The board being connected to leaves during the reset and comes back.
    let before = before.replace("! identity 303a:1001", "! identity 303a:1001 SECOND");
    let (port, _) = replay(Transcript::parse(&before).unwrap());
    // Another board of the same model, listed first, which must not be talked to.
    let (other, other_session) = replay(Transcript::parse("! identity 303a:1001 FIRST\n").unwrap());
    let (returning, session) =
        replay(Transcript::parse(&format!("! identity 303a:1001 SECOND\n{after}")).unwrap());
    let mut provider = TwoBoards {
        ports: vec![other, returning],
    };
    let mut backend =
        EspressifBackend::new(port, &mut provider, None, EspressifOptions::default()).unwrap();
    assert_eq!(block_on(backend.connect(&mut ())).unwrap().chip, "esp32-s3");
    drop(backend);
    session.finish().unwrap();
    other_session.finish().unwrap();
    assert_eq!(provider.ports.len(), 1);
}

#[test]
fn a_frame_cut_by_the_drain_does_not_swallow_the_next_answer() {
    // The second SYNC answer stops halfway; the line then goes quiet.
    let full = transcript("connect-esp32s3-usb");
    let answer = "< c0 01 08 04 00 00 00 00 00 00 00 00 00 c0\n";
    let first = full.find(answer).unwrap() + answer.len();
    let text = format!(
        "{}{}",
        &full[..first],
        full[first..].replacen(answer, "< c0 01 08 04 00\n", 1)
    );
    let (result, provider, _, _) = run_text(&text, None, EspressifOptions::default());
    assert_eq!(result.unwrap().chip, "esp32-s3");
    provider.finish().unwrap();
}

/// A replayed port that records the timeout of every read with the opcode last written.
struct TimedPort {
    inner: nanoflash::testing::ReplayPort,
    opcode: u8,
    timeouts: Rc<RefCell<Vec<(u8, Duration)>>>,
}

impl SerialPort for TimedPort {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        self.timeouts.borrow_mut().push((self.opcode, timeout));
        self.inner.read(buffer, timeout).await
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if let Some(&opcode) = data.get(2) {
            self.opcode = opcode;
        }
        self.inner.write(data).await
    }

    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError> {
        self.inner.set_baud_rate(baud_rate).await
    }

    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError> {
        self.inner.set_signals(signals).await
    }

    async fn sleep(&mut self, duration: Duration) {
        self.inner.sleep(duration).await
    }

    fn identity(&self) -> Option<UsbIdentity> {
        self.inner.identity()
    }
}

#[test]
fn each_compressed_block_waits_for_what_it_inflates_to() {
    // 4 MiB of 0xFF compress to a few blocks, each inflating to about 1 MiB.
    let image = vec![0xFF_u8; 4 * 1024 * 1024];
    let blocks = miniz_oxide::deflate::compress_to_vec_zlib(&image, 9)
        .len()
        .div_ceil(0x400);
    assert!(blocks >= 3, "{blocks}");
    let mut text = transcript("connect-esp32s3-usb");
    writeln!(
        text,
        "> c0 00 10 14 00 00 00 00 00 00 00 40 00 {blocks:02x} 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 c0\n\
         < c0 01 10 04 00 00 00 00 00 00 00 00 00 c0"
    )
    .unwrap();
    for _ in 0..blocks {
        text.push_str("> c0 00 11 .. c0\n< c0 01 11 04 00 00 00 00 00 00 00 00 00 c0\n");
    }
    let (inner, provider) = replay(Transcript::parse(&text).unwrap());
    let timeouts = Rc::new(RefCell::new(Vec::new()));
    let port = TimedPort {
        inner,
        opcode: 0,
        timeouts: Rc::clone(&timeouts),
    };
    struct Never;
    impl PortProvider for Never {
        type Port = TimedPort;
        async fn open(
            &mut self,
            _filter: &DeviceFilter,
            _baud_rate: u32,
            _timeout: Duration,
        ) -> Result<TimedPort, TransportError> {
            Err(TransportError::Disconnected)
        }
    }
    {
        let mut never = Never;
        let mut backend =
            EspressifBackend::new(port, &mut never, None, EspressifOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        let segments = [Segment {
            label: "blank",
            offset: 0,
            data: &image,
        }];
        block_on(backend.write(&segments, &mut ())).unwrap();
    }
    provider.finish().unwrap();
    let data: Vec<Duration> = timeouts
        .borrow()
        .iter()
        .filter(|(opcode, _)| *opcode == 0x11)
        .map(|(_, timeout)| *timeout)
        .collect();
    assert_eq!(data.len(), blocks);
    // 40 s per 1 000 000 bytes of the 4 MiB image would be 167.772 s for every block; each
    // block only inflates to a part of it.
    let whole = Duration::from_millis(167_772);
    assert!(
        data.iter()
            .all(|timeout| *timeout < whole && *timeout >= Duration::from_secs(10)),
        "{data:?}"
    );
}
