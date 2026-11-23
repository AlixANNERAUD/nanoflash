//! Transcript tests of the AVR backend.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use nanoflash::avr::{AvrBackend, AvrOptions};
use nanoflash::backend::{Backend, ChipInformation, ProgressEvent, Region, Segment, Stage};
use nanoflash::flash::{FlashOptions, FlashReport, Hooks, decode_intel_hex, detect, flash};
use nanoflash::manifest::{Manifest, Protocol, ResetSequence, ResetStep};
use nanoflash::testing::{
    Event, HostToken, MemorySource, RecordingProvider, ReplayProvider, Transcript, record, replay,
};
use nanoflash::transport::{Duration, UsbIdentity};
use nanoflash::{Error, ErrorClass};
use pollster::block_on;

/// The four lines of the DTR pulse in the transcripts.
const PULSE: &str = "! signals dtr=0 rts=0\n! sleep 250\n! signals dtr=1 rts=1\n! sleep 50\n";

fn transcript(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/transcripts/avr")
        .join(format!("{name}.transcript"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

struct Messages<'a>(&'a mut Vec<String>);

impl nanoflash::backend::Progress for Messages<'_> {
    fn report(&mut self, _event: ProgressEvent) {}

    fn message(&mut self, text: &str) {
        self.0.push(text.to_string());
    }
}

/// Connects over a replay of `text`.
fn connect_text(
    text: &str,
    protocol: Protocol,
    reset: Option<ResetSequence>,
    options: AvrOptions,
) -> (Result<ChipInformation, Error>, ReplayProvider, Vec<String>) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let mut messages = Vec::new();
    let result = {
        let mut backend = match AvrBackend::new(protocol, port, &mut provider, reset, options) {
            Ok(backend) => backend,
            Err(error) => panic!("{error}"),
        };
        let result = block_on(backend.connect(&mut Messages(&mut messages)));
        if let Ok(information) = &result {
            assert_eq!(
                backend.chip().map(|chip| chip.name),
                Some(&*information.chip)
            );
        } else {
            assert_eq!(backend.chip(), None);
        }
        result
    };
    (result, provider, messages)
}

/// Connects over a replay of `tests/transcripts/avr/<name>.transcript`.
fn connect(
    name: &str,
    protocol: Protocol,
    reset: Option<ResetSequence>,
    options: AvrOptions,
) -> (Result<ChipInformation, Error>, ReplayProvider, Vec<String>) {
    connect_text(&transcript(name), protocol, reset, options)
}

fn identity(vendor: u16, product: u16) -> Option<UsbIdentity> {
    Some(UsbIdentity {
        vendor,
        product,
        serial_number: None,
    })
}

#[test]
fn connects_to_an_uno() {
    let (result, provider, messages) = connect(
        "connect-uno",
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    assert_eq!(
        result.unwrap(),
        ChipInformation {
            protocol: Protocol::Stk500v1,
            chip: "atmega328p".into(),
            flash_size: Some(0x8000),
            secure_download_mode: false,
            usb: identity(0x2341, 0x0043),
            writable_from: None,
            writable_to: Some(0x7E00),
            erases_to_end: false,
        }
    );
    assert!(messages.is_empty(), "{messages:?}");
    provider.finish().unwrap();
}

#[test]
fn the_old_bootloader_is_found_at_57600() {
    let (result, provider, _) = connect(
        "connect-nano-old-bootloader",
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    let information = result.unwrap();
    assert_eq!(information.chip, "atmega328p");
    assert_eq!(information.writable_to, Some(0x7800));
    assert_eq!(information.usb, identity(0x1a86, 0x7523));
    provider.finish().unwrap();
}

#[test]
fn late_sync_replies_are_accepted() {
    let (result, provider, _) = connect(
        "connect-late-sync",
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    assert_eq!(result.unwrap().writable_to, Some(0x7E00));
    provider.finish().unwrap();
}

#[test]
fn a_chatty_sketch_is_drained() {
    let (result, provider, _) = connect(
        "connect-chatty-sketch",
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    assert_eq!(result.unwrap().chip, "atmega328p");
    provider.finish().unwrap();
}

#[test]
fn silent_port_gives_no_bootloader() {
    let (result, provider, _) = connect(
        "connect-silent",
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Stk500v1,
            rates: vec![115_200, 57_600],
        }
    );
    assert_eq!(error.class(), ErrorClass::Device);
    assert!(error.to_string().contains("DTR"), "{error}");
    provider.finish().unwrap();
}

#[test]
fn a_requested_rate_is_the_only_one_tried() {
    let text = transcript("connect-silent");
    let cut = text.find("! baud 57600").unwrap();
    let (result, provider, _) = connect_text(
        &text[..cut],
        Protocol::Stk500v1,
        None,
        AvrOptions {
            baud_rate: Some(115_200),
            ..AvrOptions::default()
        },
    );
    assert_eq!(
        result.unwrap_err(),
        Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Stk500v1,
            rates: vec![115_200],
        }
    );
    provider.finish().unwrap();
}

#[test]
fn unknown_signatures_are_named() {
    let (result, provider, _) = connect(
        "connect-unknown-signature",
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    assert_eq!(
        result.unwrap_err(),
        Error::UnknownChip {
            detail: "signature 1e 95 16".into()
        }
    );
    provider.finish().unwrap();
}

#[test]
fn an_unanswered_signature_names_the_command() {
    let text = transcript("connect-uno");
    let cut = text.find("< 14 1e 95 0f 10").unwrap();
    let (result, provider, _) = connect_text(
        &text[..cut],
        Protocol::Stk500v1,
        None,
        AvrOptions::default(),
    );
    assert_eq!(
        result.unwrap_err(),
        Error::NoResponse {
            command: "STK_READ_SIGN"
        }
    );
    provider.finish().unwrap();

    let text = transcript("connect-uno").replace("< 14 1e 95 0f 10", "< 15");
    let (result, _, _) = connect_text(&text, Protocol::Stk500v1, None, AvrOptions::default());
    match result.unwrap_err() {
        Error::InvalidResponse { command, message } => {
            assert_eq!(command, "STK_READ_SIGN");
            assert!(message.contains("STK_NOSYNC"), "{message}");
        }
        error => panic!("{error:?}"),
    }
}

#[test]
fn connects_to_a_mega_2560() {
    let (result, provider, _) = connect(
        "connect-mega2560",
        Protocol::Stk500v2,
        None,
        AvrOptions::default(),
    );
    assert_eq!(
        result.unwrap(),
        ChipInformation {
            protocol: Protocol::Stk500v2,
            chip: "atmega2560".into(),
            flash_size: Some(0x4_0000),
            secure_download_mode: false,
            usb: identity(0x2341, 0x0042),
            writable_from: None,
            writable_to: Some(0x3_C000),
            erases_to_end: false,
        }
    );
    provider.finish().unwrap();
}

#[test]
fn stk500v2_signatures_end_with_a_status() {
    // The last byte of a CMD_READ_SIGNATURE_ISP answer is a second STATUS_CMD_OK.
    let text = transcript("connect-mega2560").replace(
        "< 1b 03 00 04 0e 1b 00 98 00 91",
        "< 1b 03 00 04 0e 1b 00 98 c0 51",
    );
    let (result, _, _) = connect_text(&text, Protocol::Stk500v2, None, AvrOptions::default());
    match result.unwrap_err() {
        Error::InvalidResponse { command, message } => {
            assert_eq!(command, "CMD_READ_SIGNATURE_ISP");
            assert!(message.contains("c0"), "{message}");
        }
        error => panic!("{error:?}"),
    }
    // Noise before the start of a message is skipped.
    let text = transcript("connect-mega2560").replace(
        "< 1b 03 00 04 0e 1b 00 98 00 91",
        "< 00 ff 1b 03 00 04 0e 1b 00 98 00 91",
    );
    let (result, provider, _) =
        connect_text(&text, Protocol::Stk500v2, None, AvrOptions::default());
    assert_eq!(result.unwrap().chip, "atmega2560");
    provider.finish().unwrap();
}

#[test]
fn a_silent_mega_2560_gives_no_bootloader() {
    let attempt = format!("{PULSE}> 1b 01 00 01 0e 01 14\n");
    let text = format!("! identity 2341:0042\n! baud 115200\n{}", attempt.repeat(3));
    let (result, provider, _) =
        connect_text(&text, Protocol::Stk500v2, None, AvrOptions::default());
    assert_eq!(
        result.unwrap_err(),
        Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Stk500v2,
            rates: vec![115_200],
        }
    );
    provider.finish().unwrap();
}

#[test]
fn connects_to_a_leonardo_after_the_touch() {
    let (result, provider, messages) = connect(
        "connect-leonardo",
        Protocol::Avr109,
        None,
        AvrOptions::default(),
    );
    assert_eq!(
        result.unwrap(),
        ChipInformation {
            protocol: Protocol::Avr109,
            chip: "atmega32u4".into(),
            flash_size: Some(0x8000),
            secure_download_mode: false,
            usb: identity(0x2341, 0x0036),
            writable_from: None,
            writable_to: Some(0x7000),
            erases_to_end: false,
        }
    );
    assert!(messages.is_empty(), "{messages:?}");
    provider.finish().unwrap();
}

#[test]
fn a_leonardo_in_its_bootloader_needs_no_touch() {
    let (result, provider, _) = connect(
        "connect-leonardo-bootloader",
        Protocol::Avr109,
        Some(ResetSequence::Touch1200),
        AvrOptions::default(),
    );
    assert_eq!(result.unwrap().usb, identity(0x2341, 0x0036));
    provider.finish().unwrap();
}

#[test]
fn a_silent_caterina_gives_no_bootloader() {
    let text = "! identity 2341:0036\n> \"S\"\n< \"?\"\n> \"S\"\n> \"S\"\n";
    let (result, provider, _) = connect_text(text, Protocol::Avr109, None, AvrOptions::default());
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Avr109,
            rates: vec![],
        }
    );
    assert!(error.to_string().contains("8 s"), "{error}");
    provider.finish().unwrap();
}

#[test]
fn without_reset_nothing_is_pulsed() {
    let text = transcript("connect-uno").replace(PULSE, "");
    let (result, provider, _) = connect_text(
        &text,
        Protocol::Stk500v1,
        Some(ResetSequence::None),
        AvrOptions::default(),
    );
    assert_eq!(result.unwrap().chip, "atmega328p");
    provider.finish().unwrap();
}

#[test]
fn custom_resets_replace_the_pulse() {
    let steps = ResetSequence::Custom(vec![
        ResetStep {
            dtr: true,
            rts: false,
            wait: Duration::from_millis(100),
        },
        ResetStep {
            dtr: false,
            rts: false,
            wait: Duration::ZERO,
        },
    ]);
    let custom = "! signals dtr=1 rts=0\n! sleep 100\n! signals dtr=0 rts=0\n";
    let text = transcript("connect-uno").replace(PULSE, custom);
    let (result, provider, _) = connect_text(
        &text,
        Protocol::Stk500v1,
        Some(steps.clone()),
        AvrOptions::default(),
    );
    assert_eq!(result.unwrap().chip, "atmega328p");
    provider.finish().unwrap();

    // When no bootloader answers, the hint points at the manifest's reset sequence.
    let text = transcript("connect-silent").replace(PULSE, custom);
    let (result, provider, _) = connect_text(
        &text,
        Protocol::Stk500v1,
        Some(steps),
        AvrOptions::default(),
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::NoBootloader {
            waited_for: None,
            reset: Some("custom"),
            protocol: Protocol::Stk500v1,
            rates: vec![115_200, 57_600],
        }
    );
    assert!(error.to_string().contains("manifest"), "{error}");
    assert!(!error.to_string().contains("DTR"), "{error}");
    provider.finish().unwrap();
}

#[test]
fn resets_that_do_not_apply_are_refused() {
    for (protocol, reset) in [
        (Protocol::Stk500v1, ResetSequence::Touch1200),
        (Protocol::Stk500v2, ResetSequence::Uf2Touch1200),
        (Protocol::Avr109, ResetSequence::EspressifClassic),
    ] {
        let (port, mut provider) = replay(Transcript::default());
        let name = reset.to_string();
        let result = AvrBackend::new(
            protocol,
            port,
            &mut provider,
            Some(reset),
            AvrOptions::default(),
        );
        assert_eq!(
            result.err(),
            Some(Error::UnsupportedReset {
                protocol,
                reset: name
            })
        );
    }
    for protocol in [Protocol::Espressif, Protocol::Samba] {
        let (port, mut provider) = replay(Transcript::default());
        let result = AvrBackend::new(protocol, port, &mut provider, None, AvrOptions::default());
        assert_eq!(result.err(), Some(Error::ProtocolNotEnabled { protocol }));
    }
}

#[test]
fn avr109_ignores_the_baud_rate() {
    let (result, provider, messages) = connect(
        "connect-leonardo",
        Protocol::Avr109,
        None,
        AvrOptions {
            baud_rate: Some(115_200),
            ..AvrOptions::default()
        },
    );
    assert_eq!(result.unwrap().chip, "atmega32u4");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("ignored"), "{}", messages[0]);
    provider.finish().unwrap();
}

#[test]
fn a_board_without_reset_is_not_said_to_miss_dtr() {
    let text = transcript("connect-silent").replace(PULSE, "");
    let (result, provider, _) = connect_text(
        &text,
        Protocol::Stk500v1,
        Some(ResetSequence::None),
        AvrOptions::default(),
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::NoBootloader {
            waited_for: None,
            reset: Some("none"),
            protocol: Protocol::Stk500v1,
            rates: vec![115_200, 57_600],
        }
    );
    assert!(error.to_string().contains("\"none\""), "{error}");
    assert!(!error.to_string().contains("DTR"), "{error}");
    provider.finish().unwrap();
}

#[derive(Default)]
struct Recorder {
    events: Vec<ProgressEvent>,
    messages: Vec<String>,
}

impl nanoflash::backend::Progress for Recorder {
    fn report(&mut self, event: ProgressEvent) {
        self.events.push(event);
    }

    fn message(&mut self, text: &str) {
        self.messages.push(text.to_string());
    }
}

fn event(stage: Stage, offset: u64, done: u64, total: u64) -> ProgressEvent {
    ProgressEvent {
        stage,
        offset,
        done,
        total,
    }
}

/// The 300 bytes of `tests/images/avr-application.hex`, from offset 0.
fn application() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/avr-application.hex");
    let image = decode_intel_hex(&std::fs::read(path).unwrap(), 0x8000).unwrap();
    assert_eq!((image.start, image.bytes.len()), (0, 300));
    image.bytes
}

fn hexadecimal(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// An STK500v2 message: start, sequence, body length, token, body and the XOR of every byte
/// before the checksum.
fn message(sequence: u8, body: &[u8]) -> Vec<u8> {
    let [high, low] = u16::try_from(body.len()).unwrap().to_be_bytes();
    let mut bytes = vec![0x1b, sequence, high, low, 0x0e];
    bytes.extend_from_slice(body);
    bytes.push(bytes.iter().fold(0, |sum, byte| sum ^ byte));
    bytes
}

/// The pages of `image`, the last one padded with `0xFF`.
fn pages(image: &[u8], page_size: usize) -> Vec<Vec<u8>> {
    image
        .chunks(page_size)
        .map(|chunk| {
            let mut page = chunk.to_vec();
            page.resize(page_size, 0xFF);
            page
        })
        .collect()
}

/// The lines that follow a `connect-*` transcript to write `image` at offset 0 page by page,
/// read it back page by page and leave the bootloader, from the Protocol summary. The bytes of
/// a written page are a counted wildcard; the STK500v2 messages continue the numbering of the
/// connection (1 to 4).
fn session(protocol: Protocol, image: &[u8], page_size: usize) -> String {
    let pages = pages(image, page_size);
    let [length_high, length_low] = u16::try_from(page_size).unwrap().to_be_bytes();
    let word = |index: usize| index * page_size / 2;
    let mut text = String::new();
    match protocol {
        Protocol::Stk500v1 => {
            let load = |index: usize| {
                let [low, high] = u16::try_from(word(index)).unwrap().to_le_bytes();
                format!("> 55 {low:02x} {high:02x} 20\n< 14 10\n")
            };
            for index in 0..pages.len() {
                text.push_str(&load(index));
                writeln!(
                    text,
                    "> 64 {length_high:02x} {length_low:02x} 46 ..{page_size} 20\n< 14 10"
                )
                .unwrap();
            }
            for (index, page) in pages.iter().enumerate() {
                text.push_str(&load(index));
                writeln!(
                    text,
                    "> 74 {length_high:02x} {length_low:02x} 46 20\n< 14 {} 10",
                    hexadecimal(page)
                )
                .unwrap();
            }
            text.push_str("> 51 20\n< 14 10\n");
        }
        Protocol::Stk500v2 => {
            let mut sequence = 4_u8;
            let mut next = || {
                sequence += 1;
                sequence
            };
            let load = |sequence: u8, index: usize| {
                let word = u32::try_from(word(index)).unwrap() | 1 << 31;
                let mut body = vec![0x06];
                body.extend_from_slice(&word.to_be_bytes());
                format!(
                    "> {}\n< {}\n",
                    hexadecimal(&message(sequence, &body)),
                    hexadecimal(&message(sequence, &[0x06, 0x00]))
                )
            };
            for index in 0..pages.len() {
                text.push_str(&load(next(), index));
                let sequence = next();
                let [body_high, body_low] = u16::try_from(page_size + 10).unwrap().to_be_bytes();
                let head = [
                    0x1b,
                    sequence,
                    body_high,
                    body_low,
                    0x0e,
                    0x13,
                    length_high,
                    length_low,
                    0xc1,
                    0x0a,
                    0x40,
                    0x4c,
                    0x20,
                    0x00,
                    0x00,
                ];
                writeln!(
                    text,
                    "> {} ..{}\n< {}",
                    hexadecimal(&head),
                    page_size + 1,
                    hexadecimal(&message(sequence, &[0x13, 0x00]))
                )
                .unwrap();
            }
            for (index, page) in pages.iter().enumerate() {
                text.push_str(&load(next(), index));
                let sequence = next();
                let mut answer = vec![0x14, 0x00];
                answer.extend_from_slice(page);
                answer.push(0x00);
                writeln!(
                    text,
                    "> {}\n< {}",
                    hexadecimal(&message(sequence, &[0x14, length_high, length_low, 0x20])),
                    hexadecimal(&message(sequence, &answer))
                )
                .unwrap();
            }
            let sequence = next();
            writeln!(
                text,
                "> {}\n< {}",
                hexadecimal(&message(sequence, &[0x11, 0x01, 0x01])),
                hexadecimal(&message(sequence, &[0x11, 0x00]))
            )
            .unwrap();
        }
        Protocol::Avr109 => {
            let load = |index: usize| {
                let [high, low] = u16::try_from(word(index)).unwrap().to_be_bytes();
                format!("> \"A\" {high:02x} {low:02x}\n< \"\\r\"\n")
            };
            for index in 0..pages.len() {
                text.push_str(&load(index));
                writeln!(
                    text,
                    "> \"B\" {length_high:02x} {length_low:02x} \"F\" ..{page_size}\n< \"\\r\""
                )
                .unwrap();
            }
            for (index, page) in pages.iter().enumerate() {
                text.push_str(&load(index));
                writeln!(
                    text,
                    "> \"g\" {length_high:02x} {length_low:02x} \"F\"\n< {}",
                    hexadecimal(page)
                )
                .unwrap();
            }
            text.push_str("> \"E\"\n< \"\\r\"\n");
        }
        Protocol::Espressif | Protocol::Samba => unreachable!("not an AVR protocol"),
    }
    text
}

/// The events of a transcript, without their lines.
fn events(transcript: &Transcript) -> Vec<Event> {
    transcript
        .events
        .iter()
        .map(|entry| entry.event.clone())
        .collect()
}

/// The bytes of every write of a recorded session.
fn writes(transcript: &Transcript) -> Vec<Vec<u8>> {
    transcript
        .events
        .iter()
        .filter_map(|entry| match &entry.event {
            Event::Host(tokens) => Some(
                tokens
                    .iter()
                    .map(|token| match token {
                        HostToken::Byte(byte) => *byte,
                        HostToken::Any | HostToken::Count(_) => {
                            panic!("a recording has no wildcard")
                        }
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

/// The data of every `STK_PROG_PAGE`, `CMD_PROGRAM_FLASH_ISP` or AVR109 `B` of a recorded
/// session.
fn programmed_pages(transcript: &Transcript, protocol: Protocol) -> Vec<Vec<u8>> {
    writes(transcript)
        .into_iter()
        .filter_map(|bytes| match (protocol, bytes.as_slice()) {
            (Protocol::Stk500v1, [0x64, _, _, 0x46, data @ .., 0x20]) => Some(data.to_vec()),
            // The header, then the command and its nine parameter bytes; the checksum last.
            (
                Protocol::Stk500v2,
                [
                    0x1b,
                    _,
                    _,
                    _,
                    0x0e,
                    0x13,
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    rest @ ..,
                ],
            ) => rest.split_last().map(|(_, data)| data.to_vec()),
            (Protocol::Avr109, [b'B', _, _, b'F', data @ ..]) => Some(data.to_vec()),
            _ => None,
        })
        .collect()
}

/// Connects over a replay of `text` through `Backend::connect`, then runs `body`.
fn run<T>(
    text: &str,
    protocol: Protocol,
    body: impl FnOnce(&mut AvrBackend<'_, ReplayProvider>, &mut Recorder) -> T,
) -> (T, ReplayProvider, Recorder) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let mut recorder = Recorder::default();
    let result = {
        let mut backend =
            AvrBackend::new(protocol, port, &mut provider, None, AvrOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        body(&mut backend, &mut recorder)
    };
    (result, provider, recorder)
}

/// [`run`], recording what passes on the port.
fn run_recorded<T>(
    text: &str,
    protocol: Protocol,
    body: impl FnOnce(&mut AvrBackend<'_, RecordingProvider<ReplayProvider>>) -> T,
) -> (T, RecordingProvider<ReplayProvider>) {
    let (port, provider) = replay(Transcript::parse(text).unwrap());
    let (port, mut provider) = record(port, provider);
    let result = {
        let mut backend =
            AvrBackend::new(protocol, port, &mut provider, None, AvrOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        body(&mut backend)
    };
    (result, provider)
}

/// Writes, verifies and resets `application()` over `connect-<board>` and `write-<board>`.
fn writes_and_verifies(board: &str, protocol: Protocol, done: &[u64]) {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &image,
    };
    let text = transcript(&format!("connect-{board}")) + &transcript(&format!("write-{board}"));
    let (result, provider, recorder) = run(&text, protocol, |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))?;
        block_on(backend.reset_to_application())
    });
    result.unwrap();
    provider.finish().unwrap();
    let expected: Vec<ProgressEvent> = [Stage::Writing, Stage::Verifying]
        .into_iter()
        .flat_map(|stage| done.iter().map(move |done| event(stage, 0, *done, 300)))
        .collect();
    assert_eq!(recorder.events, expected);
    assert!(recorder.messages.is_empty(), "{:?}", recorder.messages);
}

#[test]
fn writes_and_verifies_an_uno() {
    writes_and_verifies("uno", Protocol::Stk500v1, &[128, 256, 300]);
}

#[test]
fn writes_and_verifies_a_mega_2560() {
    writes_and_verifies("mega2560", Protocol::Stk500v2, &[256, 300]);
}

#[test]
fn writes_and_verifies_a_leonardo() {
    writes_and_verifies("leonardo", Protocol::Avr109, &[128, 256, 300]);
}

#[test]
fn committed_write_transcripts_match_the_protocol_summary() {
    let image = application();
    for (name, protocol, page_size) in [
        ("write-uno", Protocol::Stk500v1, 128),
        ("write-mega2560", Protocol::Stk500v2, 256),
        ("write-leonardo", Protocol::Avr109, 128),
    ] {
        let committed = Transcript::parse(&transcript(name)).unwrap();
        let expected = Transcript::parse(&session(protocol, &image, page_size)).unwrap();
        assert_eq!(committed.identity, None, "{name}");
        assert_eq!(events(&committed), events(&expected), "{name}");
    }
}

#[test]
fn sends_the_pages_as_given() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &image,
    };
    for (board, protocol, page_size) in [
        ("uno", Protocol::Stk500v1, 128),
        ("mega2560", Protocol::Stk500v2, 256),
        ("leonardo", Protocol::Avr109, 128),
    ] {
        let text = transcript(&format!("connect-{board}")) + &transcript(&format!("write-{board}"));
        let (result, provider) = run_recorded(&text, protocol, |backend| {
            block_on(backend.write(&[segment], &mut ()))?;
            block_on(backend.verify(&[segment], &mut ()))?;
            block_on(backend.reset_to_application())
        });
        result.unwrap();
        provider.inner().finish().unwrap();
        assert_eq!(
            programmed_pages(&provider.transcript(), protocol),
            pages(&image, page_size),
            "{board}"
        );
    }
}

#[test]
fn pads_the_last_page_and_checks_the_image_bytes_only() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &image,
    };
    // The padding of the last page reads back as 0x00: only the 44 image bytes are compared.
    let last = &pages(&image, 128)[2];
    let mut zeroed = last.clone();
    zeroed[44..].fill(0x00);
    let original = transcript("connect-uno") + &transcript("write-uno");
    let text = original.replace(
        &format!("< 14 {} 10", hexadecimal(last)),
        &format!("< 14 {} 10", hexadecimal(&zeroed)),
    );
    assert_ne!(text, original);
    let (result, provider, _) = run(&text, Protocol::Stk500v1, |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))?;
        block_on(backend.reset_to_application())
    });
    result.unwrap();
    provider.finish().unwrap();

    // A one-byte image programs one whole page.
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &[0xAB],
    };
    let text = transcript("connect-uno") + &session(Protocol::Stk500v1, &[0xAB], 128);
    let (result, provider) = run_recorded(&text, Protocol::Stk500v1, |backend| {
        block_on(backend.write(&[segment], &mut ()))?;
        block_on(backend.verify(&[segment], &mut ()))?;
        block_on(backend.reset_to_application())
    });
    result.unwrap();
    provider.inner().finish().unwrap();
    let mut page = vec![0xAB];
    page.resize(128, 0xFF);
    assert_eq!(
        programmed_pages(&provider.transcript(), Protocol::Stk500v1),
        [page]
    );
}

#[test]
fn a_differing_byte_is_reported() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &image,
    };
    let page = &image[0x80..0x100];
    let mut changed = page.to_vec();
    changed[5] = 0xff;
    let original = transcript("connect-uno") + &transcript("write-uno");
    let text = original.replace(
        &format!("< 14 {} 10", hexadecimal(page)),
        &format!("< 14 {} 10", hexadecimal(&changed)),
    );
    assert_ne!(text, original);
    let (result, _, _) = run(&text, Protocol::Stk500v1, |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))
    });
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::ReadBackDiffers {
            label: "application".into(),
            offset: 0x85,
            expected: 0xa4,
            actual: 0xff,
        }
    );
    assert_eq!(error.class(), ErrorClass::Verification);
}

/// What `body` returns after `connect-<board>` followed by `lines`.
fn after_connect<T>(
    board: &str,
    protocol: Protocol,
    lines: &str,
    body: impl FnOnce(&mut AvrBackend<'_, ReplayProvider>, &mut Recorder) -> T,
) -> T {
    let text = transcript(&format!("connect-{board}")) + lines;
    run(&text, protocol, body).0
}

fn invalid_response(error: Error, expected: &str) -> String {
    match error {
        Error::InvalidResponse { command, message } => {
            assert_eq!(command, expected, "{message}");
            message
        }
        error => panic!("{error:?}"),
    }
}

#[test]
fn malformed_answers_name_the_command() {
    let image = application();
    let page = Segment {
        label: "application",
        offset: 0,
        data: &image[..128],
    };

    let lines = "> 55 00 00 20\n< 14 10\n> 64 00 80 46 ..128 20\n< 15\n";
    let error = after_connect("uno", Protocol::Stk500v1, lines, |backend, progress| {
        block_on(backend.write(&[page], progress))
    })
    .unwrap_err();
    let detail = invalid_response(error, "STK_PROG_PAGE");
    assert!(detail.contains("STK_NOSYNC"), "{detail}");

    let lines = format!(
        "> 55 00 00 20\n< 14 10\n> 74 00 80 46 20\n< 14 {}\n",
        hexadecimal(&image[..128])
    );
    let error = after_connect("uno", Protocol::Stk500v1, &lines, |backend, progress| {
        block_on(backend.verify(&[page], progress))
    })
    .unwrap_err();
    invalid_response(error, "STK_READ_PAGE");

    // Three bytes of noise, then an answer whose checksum should be 02.
    let lines = format!(
        "> {}\n< {}\n> 1b 06 01 0a 0e 13 01 00 c1 0a 40 4c 20 00 00 ..257\n< 00 11 22 1b 06 00 02 0e 13 00 ff\n",
        hexadecimal(&message(5, &[0x06, 0x80, 0x00, 0x00, 0x00])),
        hexadecimal(&message(5, &[0x06, 0x00]))
    );
    let error = after_connect(
        "mega2560",
        Protocol::Stk500v2,
        &lines,
        |backend, progress| block_on(backend.write(&[page], progress)),
    )
    .unwrap_err();
    let detail = invalid_response(error, "CMD_PROGRAM_FLASH_ISP");
    assert!(detail.contains("checksum"), "{detail}");

    let lines = "> \"A\" 00 00\n< \"\\r\"\n> \"B\" 00 80 \"F\" ..128\n< \"?\"\n";
    let error = after_connect("leonardo", Protocol::Avr109, lines, |backend, progress| {
        block_on(backend.write(&[page], progress))
    })
    .unwrap_err();
    invalid_response(error, "AVR109 B");

    let error = after_connect("uno", Protocol::Stk500v1, "> 51 20\n", |backend, _| {
        block_on(backend.reset_to_application())
    })
    .unwrap_err();
    assert_eq!(
        error,
        Error::NoResponse {
            command: "STK_LEAVE_PROGMODE"
        }
    );

    // A device that sends 0x41 forever after STK_READ_PAGE.
    let lines = format!(
        "> 55 00 00 20\n< 14 10\n> 74 00 80 46 20\n< {}\n",
        hexadecimal(&[0x41; 4096])
    );
    let text = transcript("connect-uno") + &lines;
    let (result, provider) = run_recorded(&text, Protocol::Stk500v1, |backend| {
        block_on(backend.verify(&[page], &mut ()))
    });
    invalid_response(result.unwrap_err(), "STK_READ_PAGE");
    let recorded = provider.transcript();
    let after_read = recorded
        .events
        .iter()
        .rposition(|entry| matches!(&entry.event, Event::Host(tokens) if tokens.first() == Some(&HostToken::Byte(0x74))))
        .unwrap();
    let received: usize = recorded.events[after_read..]
        .iter()
        .map(|entry| match &entry.event {
            Event::Device(bytes) => bytes.len(),
            _ => 0,
        })
        .sum();
    assert!(received <= 128 + 2 + 64, "{received} bytes read");
}

#[test]
fn erasing_programs_erased_pages() {
    let erased = hexadecimal(&[0xFF; 128]);
    let lines = format!(
        "> 55 80 3e 20\n< 14 10\n> 64 00 80 46 {erased} 20\n< 14 10\n> 55 c0 3e 20\n< 14 10\n> 64 00 80 46 {erased} 20\n< 14 10\n"
    );
    let text = transcript("connect-uno") + &lines;
    let region = Region {
        offset: 0x7D00,
        size: 0x100,
    };
    let (result, provider, recorder) = run(&text, Protocol::Stk500v1, |backend, progress| {
        block_on(backend.erase(region, progress))
    });
    result.unwrap();
    provider.finish().unwrap();
    // The orchestration reports the start and the end of the region: only the pages between.
    assert_eq!(
        recorder.events,
        [event(Stage::Erasing, 0x7D00, 0x80, 0x100)]
    );
}

#[test]
fn pages_above_the_bootloader_are_refused_before_sending() {
    let data = [0x00; 0x80];
    let segment = Segment {
        label: "application",
        offset: 0x7E00,
        data: &data,
    };
    let (result, provider, _) = run(
        &transcript("connect-uno"),
        Protocol::Stk500v1,
        |backend, progress| {
            let write = block_on(backend.write(&[segment], progress));
            let verify = block_on(backend.verify(&[segment], progress));
            let region = Region {
                offset: 0x7D80,
                size: 0x100,
            };
            let erase = block_on(backend.erase(region, progress));
            (write, verify, erase)
        },
    );
    let above = Error::AboveWritableFlash {
        what: "image \"application\"".into(),
        end: 0x7E80,
        writable_to: 0x7E00,
        protocol: Protocol::Stk500v1,
    };
    assert_eq!(result.0, Err(above.clone()));
    assert_eq!(result.1, Err(above));
    assert_eq!(
        result.2,
        Err(Error::AboveWritableFlash {
            what: "erase region at 0x7d80".into(),
            end: 0x7E80,
            writable_to: 0x7E00,
            protocol: Protocol::Stk500v1,
        })
    );
    provider.finish().unwrap();
}

#[test]
fn misaligned_segments_are_refused_before_sending() {
    let data = [0x00; 0x10];
    let segment = Segment {
        label: "application",
        offset: 0x40,
        data: &data,
    };
    let (result, provider, _) = run(
        &transcript("connect-uno"),
        Protocol::Stk500v1,
        |backend, progress| {
            let write = block_on(backend.write(&[segment], progress));
            let region = Region {
                offset: 0x80,
                size: 0x40,
            };
            let erase = block_on(backend.erase(region, progress));
            (write, erase)
        },
    );
    assert_eq!(
        result.0,
        Err(Error::MisalignedImage {
            label: "application".into(),
            offset: 0x40,
            required: 0x80,
        })
    );
    assert_eq!(
        result.1,
        Err(Error::MisalignedErase {
            entry: "region at 0x80".into(),
            offset: 0x80,
            size: 0x40,
            required: 0x80,
        })
    );
    provider.finish().unwrap();
}

#[test]
fn a_disconnect_after_exit_is_success() {
    let lines = "> \"E\"\n! disconnect\n";
    let text = transcript("connect-leonardo") + lines;
    let (result, provider, _) = run(&text, Protocol::Avr109, |backend, _| {
        block_on(backend.reset_to_application())
    });
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn nothing_is_done_before_connecting() {
    for protocol in [Protocol::Stk500v1, Protocol::Stk500v2, Protocol::Avr109] {
        let (port, mut provider) = replay(Transcript::default());
        let mut backend =
            AvrBackend::new(protocol, port, &mut provider, None, AvrOptions::default()).unwrap();
        let segment = Segment {
            label: "application",
            offset: 0,
            data: &[0; 4],
        };
        let region = Region {
            offset: 0,
            size: 0x100,
        };
        let none = Err(Error::NotConnected { protocol });
        assert_eq!(block_on(backend.write(&[segment], &mut ())), none);
        assert_eq!(block_on(backend.verify(&[segment], &mut ())), none);
        assert_eq!(block_on(backend.erase(region, &mut ())), none);
        assert_eq!(block_on(backend.reset_to_application()), none);
        drop(backend);
        provider.finish().unwrap();
    }
}

/// The STK500v2 lines that program the page at `offset` with any 256 bytes, as messages
/// `sequence` and `sequence + 1`.
fn mega_program_lines(sequence: u8, offset: u64) -> String {
    let word = u32::try_from(offset / 2).unwrap() | 1 << 31;
    let mut load = vec![0x06];
    load.extend_from_slice(&word.to_be_bytes());
    let program = sequence.wrapping_add(1);
    format!(
        "> {}\n< {}\n> 1b {program:02x} 01 0a 0e 13 01 00 c1 0a 40 4c 20 00 00 ..257\n< {}\n",
        hexadecimal(&message(sequence, &load)),
        hexadecimal(&message(sequence, &[0x06, 0x00])),
        hexadecimal(&message(program, &[0x13, 0x00]))
    )
}

#[test]
fn the_mega_2560_programs_pages_in_erase_order() {
    let image = application();
    // The wiring bootloader erases page 0 at the first page programmed, wherever it goes.
    let high = Segment {
        label: "application",
        offset: 0x100,
        data: &image,
    };
    let (result, provider, _) = run(
        &transcript("connect-mega2560"),
        Protocol::Stk500v2,
        |backend, progress| block_on(backend.write(&[high], progress)),
    );
    assert_eq!(
        result,
        Err(Error::PageOutOfEraseOrder {
            what: "image \"application\"".into(),
            offset: 0x100,
            erases_next: 0,
        })
    );
    provider.finish().unwrap();

    // Two images with a gap between them.
    let first = Segment {
        label: "first",
        offset: 0,
        data: &image[..0x100],
    };
    let second = Segment {
        label: "second",
        offset: 0x200,
        data: &image[..0x10],
    };
    let (result, provider, _) = run(
        &transcript("connect-mega2560"),
        Protocol::Stk500v2,
        |backend, progress| block_on(backend.write(&[first, second], progress)),
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::PageOutOfEraseOrder {
            what: "image \"second\"".into(),
            offset: 0x200,
            erases_next: 0x100,
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    provider.finish().unwrap();

    // An erase region that does not start at 0.
    let region = Region {
        offset: 0x1000,
        size: 0x100,
    };
    let (result, provider, _) = run(
        &transcript("connect-mega2560"),
        Protocol::Stk500v2,
        |backend, progress| block_on(backend.erase(region, progress)),
    );
    assert_eq!(
        result,
        Err(Error::PageOutOfEraseOrder {
            what: "erase region at 0x1000".into(),
            offset: 0x1000,
            erases_next: 0,
        })
    );
    provider.finish().unwrap();

    // Two images in one run from 0 are written.
    let second = Segment {
        label: "second",
        offset: 0x100,
        data: &image[..0x10],
    };
    let lines = mega_program_lines(5, 0) + &mega_program_lines(7, 0x100);
    let text = transcript("connect-mega2560") + &lines;
    let (result, provider, _) = run(&text, Protocol::Stk500v2, |backend, progress| {
        block_on(backend.write(&[first, second], progress))
    });
    result.unwrap();
    provider.finish().unwrap();

    // Other bootloaders erase the page they program: an Uno writes at 0x80 first.
    let lines = "> 55 40 00 20\n< 14 10\n> 64 00 80 46 ..128 20\n< 14 10\n";
    let text = transcript("connect-uno") + lines;
    let segment = Segment {
        label: "application",
        offset: 0x80,
        data: &image[..0x80],
    };
    let (result, provider, _) = run(&text, Protocol::Stk500v1, |backend, progress| {
        block_on(backend.write(&[segment], progress))
    });
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn the_mega_2560_writes_anywhere_after_erasing_everything() {
    let image = application();
    let mut lines = String::new();
    let mut sequence = 5_u8;
    let mut offset = 0;
    while offset < 0x3_C000 {
        lines.push_str(&mega_program_lines(sequence, offset));
        sequence = sequence.wrapping_add(2);
        offset += 0x100;
    }
    for page in [0x1000, 0x1100] {
        lines.push_str(&mega_program_lines(sequence, page));
        sequence = sequence.wrapping_add(2);
    }
    let text = transcript("connect-mega2560") + &lines;
    let segment = Segment {
        label: "application",
        offset: 0x1000,
        data: &image,
    };
    let (result, provider, _) = run(&text, Protocol::Stk500v2, |backend, progress| {
        let region = Region {
            offset: 0,
            size: 0x3_C000,
        };
        block_on(backend.erase(region, progress))?;
        block_on(backend.write(&[segment], progress))?;
        // Those pages were programmed since the erase: writing them again needs an erase.
        block_on(backend.write(&[segment], progress))
    });
    assert_eq!(
        result,
        Err(Error::PageOutOfEraseOrder {
            what: "image \"application\"".into(),
            offset: 0x1000,
            erases_next: 0x3_C000,
        })
    );
    provider.finish().unwrap();
}

#[test]
fn a_failed_page_on_the_mega_2560_needs_a_new_connection() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &image,
    };
    // The answer to CMD_PROGRAM_FLASH_ISP never comes: where the bootloader erases next is
    // unknown.
    let lines = format!(
        "> {}\n< {}\n> 1b 06 01 0a 0e 13 01 00 c1 0a 40 4c 20 00 00 ..257\n",
        hexadecimal(&message(5, &[0x06, 0x80, 0x00, 0x00, 0x00])),
        hexadecimal(&message(5, &[0x06, 0x00]))
    );
    let text = transcript("connect-mega2560") + &lines;
    let (result, provider, _) = run(&text, Protocol::Stk500v2, |backend, progress| {
        let first = block_on(backend.write(&[segment], progress));
        let second = block_on(backend.write(&[segment], progress));
        (first, second)
    });
    assert_eq!(
        result.0,
        Err(Error::NoResponse {
            command: "CMD_PROGRAM_FLASH_ISP"
        })
    );
    assert_eq!(
        result.1,
        Err(Error::NotConnected {
            protocol: Protocol::Stk500v2
        })
    );
    provider.finish().unwrap();
}

#[test]
fn the_mega_2560_writes_nothing_where_its_bootloader_does_not_erase() {
    // The wiring bootloader erases no page from 0x3C000, 8 KiB below itself.
    let data = [0x00; 0x200];
    let segment = Segment {
        label: "application",
        offset: 0x3_BF00,
        data: &data,
    };
    let (result, provider, _) = run(
        &transcript("connect-mega2560"),
        Protocol::Stk500v2,
        |backend, progress| {
            let write = block_on(backend.write(&[segment], progress));
            let region = Region {
                offset: 0x3_C000,
                size: 0x2000,
            };
            let erase = block_on(backend.erase(region, progress));
            (write, erase)
        },
    );
    let error = result.0.unwrap_err();
    assert_eq!(
        error,
        Error::AboveWritableFlash {
            what: "image \"application\"".into(),
            end: 0x3_C100,
            writable_to: 0x3_C000,
            protocol: Protocol::Stk500v2,
        }
    );
    assert!(error.to_string().contains("16 KiB"), "{error}");
    assert_eq!(
        result.1,
        Err(Error::AboveWritableFlash {
            what: "erase region at 0x3c000".into(),
            end: 0x3_E000,
            writable_to: 0x3_C000,
            protocol: Protocol::Stk500v2,
        })
    );
    provider.finish().unwrap();
}

fn fixture(relative: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The text of `tests/boards/<name>.toml`.
fn board_text(name: &str) -> String {
    String::from_utf8(fixture(&format!("boards/{name}.toml"))).unwrap()
}

fn board(name: &str) -> Manifest {
    Manifest::from_toml(&board_text(name)).unwrap()
}

/// The layout and the image of the fixture boards, under the paths their manifests use.
fn fixture_source() -> MemorySource {
    let mut files = BTreeMap::new();
    for path in ["../layouts/uno.toml", "../images/avr-application.hex"] {
        files.insert(path.to_string(), fixture(&path[3..]));
    }
    MemorySource { files }
}

/// Flashes `manifest` over a replay of `text`.
fn run_flash(
    manifest: &Manifest,
    text: &str,
    source: &mut MemorySource,
    progress: &mut Recorder,
) -> (Result<FlashReport, Error>, ReplayProvider) {
    run_flash_with(manifest, text, source, progress, &FlashOptions::default())
}

/// [`run_flash`] with `options`.
fn run_flash_with(
    manifest: &Manifest,
    text: &str,
    source: &mut MemorySource,
    progress: &mut Recorder,
    options: &FlashOptions,
) -> (Result<FlashReport, Error>, ReplayProvider) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let hooks = Hooks {
        source,
        verifier: &mut (),
        progress,
    };
    let result = block_on(flash(manifest, port, &mut provider, hooks, options));
    (result, provider)
}

fn images(report: &FlashReport) -> Vec<(&str, u64, u64)> {
    report
        .images
        .iter()
        .map(|image| (image.label.as_str(), image.offset, image.size))
        .collect()
}

fn stages(recorder: &Recorder) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();
    for event in &recorder.events {
        if stages.last() != Some(&event.stage) {
            stages.push(event.stage);
        }
    }
    stages
}

/// Flashes the fixture board `name` over `flash-<transcript>` and checks the run: connected,
/// written, verified and reset, with nothing left of the transcript and no message.
fn flashes(name: &str, transcript_name: &str) -> FlashReport {
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &board(name),
        &transcript(&format!("flash-{transcript_name}")),
        &mut fixture_source(),
        &mut recorder,
    );
    let report = result.unwrap();
    provider.finish().unwrap();
    assert!(report.verified);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(
        stages(&recorder),
        [
            Stage::Connecting,
            Stage::Writing,
            Stage::Verifying,
            Stage::Resetting
        ]
    );
    assert!(recorder.messages.is_empty(), "{:?}", recorder.messages);
    report
}

#[test]
fn committed_flash_and_detect_transcripts_are_their_parts() {
    let leave_uno = "> 51 20\n< 14 10\n";
    let leave_leonardo = "> \"E\"\n< \"\\r\"\n";
    for (name, parts) in [
        ("flash-uno", ["connect-uno", "write-uno"]),
        ("flash-nano", ["connect-nano-old-bootloader", "write-uno"]),
        ("flash-mega2560", ["connect-mega2560", "write-mega2560"]),
        ("flash-leonardo", ["connect-leonardo", "write-leonardo"]),
    ] {
        let committed = Transcript::parse(&transcript(name)).unwrap();
        let expected = Transcript::parse(&(transcript(parts[0]) + &transcript(parts[1]))).unwrap();
        assert_eq!(committed.identity, expected.identity, "{name}");
        assert_eq!(events(&committed), events(&expected), "{name}");
    }
    for (name, connect, leave) in [
        ("detect-uno", "connect-uno", leave_uno),
        ("detect-leonardo", "connect-leonardo", leave_leonardo),
    ] {
        let committed = Transcript::parse(&transcript(name)).unwrap();
        let expected = Transcript::parse(&(transcript(connect) + leave)).unwrap();
        assert_eq!(committed.identity, expected.identity, "{name}");
        assert_eq!(events(&committed), events(&expected), "{name}");
    }
    // The read-back transcript is flash-uno with byte 5 of the second page read changed, which
    // ends the verification: the third page is not read, and STK_LEAVE_PROGMODE follows.
    let image = application();
    let page = &image[0x80..0x100];
    let mut changed = page.to_vec();
    changed[5] = 0xff;
    let third = format!(
        "> 55 80 00 20\n< 14 10\n> 74 00 80 46 20\n< 14 {} 10\n",
        hexadecimal(&pages(&image, 128)[2])
    );
    let flash_uno = transcript("flash-uno");
    let read_back = flash_uno
        .replace(
            &format!("< 14 {} 10", hexadecimal(page)),
            &format!("< 14 {} 10", hexadecimal(&changed)),
        )
        .replacen(&third, "", 1);
    assert_eq!(flash_uno.matches(&third).count(), 1);
    assert_eq!(
        events(&Transcript::parse(&transcript("flash-uno-read-back")).unwrap()),
        events(&Transcript::parse(&read_back).unwrap())
    );
}

#[test]
fn flashes_the_uno_fixture() {
    let report = flashes("uno", "uno");
    assert_eq!(report.board, "arduino-uno-r3");
    assert_eq!(report.chip.protocol, Protocol::Stk500v1);
    assert_eq!(report.chip.chip, "atmega328p");
    assert_eq!(report.chip.writable_to, Some(0x7E00));
    assert_eq!(report.chip.usb, identity(0x2341, 0x0043));
    assert_eq!(images(&report), [("application", 0, 300)]);
}

#[test]
fn flashes_a_nano_clone_behind_a_bridge() {
    // 1a86:7523 is a CH340: no board lists it, and the Nano, which lists no identity, suits a
    // bridge. Its old bootloader answers at 57600 and keeps 2 KiB.
    let report = flashes("nano", "nano");
    assert_eq!(report.board, "arduino-nano");
    assert_eq!(report.chip.chip, "atmega328p");
    assert_eq!(report.chip.usb, identity(0x1a86, 0x7523));
    assert_eq!(report.chip.writable_to, Some(0x7800));
    assert_eq!(images(&report), [("image at 0x0", 0, 300)]);
}

#[test]
fn flashes_a_mega_2560() {
    let report = flashes("mega2560", "mega2560");
    assert_eq!(report.board, "arduino-mega-2560");
    assert_eq!(report.chip.protocol, Protocol::Stk500v2);
    assert_eq!(report.chip.chip, "atmega2560");
    assert_eq!(report.chip.flash_size, Some(0x4_0000));
    assert_eq!(report.chip.writable_to, Some(0x3_C000));
    assert_eq!(images(&report), [("image at 0x0", 0, 300)]);
}

#[test]
fn flashes_a_leonardo_through_the_touch() {
    // The bootloader devices come from the manifest: the board reappears as 2341:0036.
    let report = flashes("leonardo", "leonardo");
    assert_eq!(report.board, "arduino-leonardo");
    assert_eq!(report.chip.protocol, Protocol::Avr109);
    assert_eq!(report.chip.chip, "atmega32u4");
    assert_eq!(report.chip.usb, identity(0x2341, 0x0036));
    assert_eq!(report.chip.writable_to, Some(0x7000));
    assert_eq!(images(&report), [("image at 0x0", 0, 300)]);

    // A Leonardo of the 2A03 vendor reappears as the manifest's 2a03:0036.
    let text = transcript("flash-leonardo")
        .replace("! identity 2341:8036", "! identity 2a03:8036")
        .replace("! reopen 2341:0036", "! reopen 2a03:0036");
    let (result, provider) = run_flash(
        &board("leonardo"),
        &text,
        &mut fixture_source(),
        &mut Recorder::default(),
    );
    assert_eq!(result.unwrap().chip.usb, identity(0x2a03, 0x0036));
    provider.finish().unwrap();
}

#[test]
fn a_read_back_difference_still_resets() {
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &board("uno"),
        &transcript("flash-uno-read-back"),
        &mut fixture_source(),
        &mut recorder,
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::ReadBackDiffers {
            label: "application".into(),
            offset: 0x85,
            expected: 0xa4,
            actual: 0xff,
        }
    );
    assert_eq!(error.class(), ErrorClass::Verification);
    // STK_LEAVE_PROGMODE was sent and answered: nothing is left of the transcript.
    provider.finish().unwrap();
    assert_eq!(stages(&recorder).last(), Some(&Stage::Resetting));
    assert!(recorder.messages.is_empty(), "{:?}", recorder.messages);
}

#[test]
fn with_bootloader_hex_is_refused_before_writing() {
    // Arduino's `.with_bootloader.hex`: the sketch, then Optiboot up to its version bytes
    // (4.4) at 0x7FFE, so the image spans the whole 32 KiB.
    let hex = String::from_utf8(fixture("images/avr-application.hex")).unwrap();
    let hex = hex.replace(":00000001FF", ":027FFE00040479\n:00000001FF");
    let text = board_text("uno")
        .replace("../images/avr-application.hex", "with-bootloader.hex")
        .replace(
            "sha256 = \"c4ba8536e262509cb35fbe5da801c89d3695d5f5d46c99cccae49aae6b8152da\"\n",
            "",
        );
    let manifest = Manifest::from_toml(&text).unwrap();
    let mut source = fixture_source();
    source
        .files
        .insert("with-bootloader.hex".to_string(), hex.into_bytes());
    let mut recorder = Recorder::default();
    // The device is never touched: the replay holds its identity only.
    let (result, provider) = run_flash(
        &manifest,
        "! identity 2341:0043\n",
        &mut source,
        &mut recorder,
    );
    assert_eq!(
        result.unwrap_err(),
        Error::ImageTooLarge {
            label: "application".into(),
            partition: "application".into(),
            image_size: 0x8000,
            partition_size: 0x7E00,
        }
    );
    provider.finish().unwrap();
    assert!(recorder.events.is_empty(), "{:?}", recorder.events);
}

/// The Uno test board without its layout (a layout-less STK500v1 board), writing `image.bin` at
/// 0x0.
fn layout_less_uno_with(image: &[u8]) -> (Manifest, MemorySource) {
    let original = board_text("uno");
    let text = original
        .replace("layout = \"../layouts/uno.toml\"\n", "")
        .replace("partition = \"application\"", "offset = 0")
        .replace("../images/avr-application.hex", "image.bin")
        .replace(
            "sha256 = \"c4ba8536e262509cb35fbe5da801c89d3695d5f5d46c99cccae49aae6b8152da\"\n",
            "",
        );
    assert!(
        !text.contains("layout") && !text.contains("sha256"),
        "{text}"
    );
    let mut source = MemorySource::default();
    source.files.insert("image.bin".to_string(), image.to_vec());
    (Manifest::from_toml(&text).unwrap(), source)
}

#[test]
fn image_ending_at_the_bootloader() {
    // Up to 0x7E00, where Optiboot starts: 252 pages, written and read back.
    let image: Vec<u8> = (0..0x7E00_u32).map(|index| (index % 251) as u8).collect();
    let (manifest, mut source) = layout_less_uno_with(&image);
    let text = transcript("connect-uno") + &session(Protocol::Stk500v1, &image, 0x80);
    let (result, provider) = run_flash(&manifest, &text, &mut source, &mut Recorder::default());
    let report = result.unwrap();
    assert_eq!(images(&report), [("image at 0x0", 0, 0x7E00)]);
    assert!(report.verified);
    provider.finish().unwrap();

    // One page more reaches Optiboot: refused once the chip is known, before any page is
    // programmed, and the board is reset.
    let mut longer = image;
    longer.resize(0x7E80, 0xA5);
    let (manifest, mut source) = layout_less_uno_with(&longer);
    let text = transcript("connect-uno") + "> 51 20\n< 14 10\n";
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(&manifest, &text, &mut source, &mut recorder);
    assert_eq!(
        result.unwrap_err(),
        Error::AboveWritableFlash {
            what: "image \"image at 0x0\"".into(),
            end: 0x7E80,
            writable_to: 0x7E00,
            protocol: Protocol::Stk500v1,
        }
    );
    provider.finish().unwrap();
    assert_eq!(stages(&recorder), [Stage::Connecting, Stage::Resetting]);
}

#[test]
fn detects_without_a_manifest() {
    for (name, protocol, chip, usb) in [
        // 2341:0043 is an Uno: STK500v1 at 115200.
        (
            "detect-uno",
            Protocol::Stk500v1,
            "atmega328p",
            identity(0x2341, 0x0043),
        ),
        // 2341:8036 is a Leonardo: AVR109 after the touch, on the built-in 2341:0036.
        (
            "detect-leonardo",
            Protocol::Avr109,
            "atmega32u4",
            identity(0x2341, 0x0036),
        ),
    ] {
        let (port, mut provider) = replay(Transcript::parse(&transcript(name)).unwrap());
        let information = block_on(detect(port, &mut provider, None, &mut ())).unwrap();
        assert_eq!(information.protocol, protocol, "{name}");
        assert_eq!(information.chip, chip, "{name}");
        assert_eq!(information.usb, usb, "{name}");
        provider.finish().unwrap();
    }
}

#[test]
fn a_bridge_needs_a_protocol() {
    // A CH340 may carry any board: nothing is sent before the protocol is known.
    let (port, mut provider) = replay(Transcript::parse("! identity 1a86:7523\n").unwrap());
    let error = block_on(detect(port, &mut provider, None, &mut ())).unwrap_err();
    assert_eq!(
        error,
        Error::ProtocolRequired {
            identity: identity(0x1a86, 0x7523),
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    provider.finish().unwrap();
}

/// A Mega 2560 board without a layout that writes `first.bin` (one page of `0x00`) at 0x0 and
/// `second.bin` (16 bytes of `0x11`) at `second`, with `extra` at the end of the board; and the
/// source of both images.
fn mega_two_images(second: u64, extra: &str) -> (Manifest, MemorySource) {
    let text = format!(
        "[[board]]\nidentifier = \"mega\"\nprotocol = \"stk500v2\"\nchip = \"atmega2560\"\n\
         application-devices = [{{ vendor = 0x2341, product = 0x0042 }}]\n\
         [[board.image]]\noffset = 0\npath = \"first.bin\"\n\
         [[board.image]]\noffset = 0x{second:x}\npath = \"second.bin\"\n{extra}"
    );
    let mut source = MemorySource::default();
    source.files.insert("first.bin".into(), vec![0x00; 0x100]);
    source.files.insert("second.bin".into(), vec![0x11; 0x10]);
    (Manifest::from_toml(&text).unwrap(), source)
}

/// The STK500v2 lines of `CMD_LEAVE_PROGMODE_ISP` as message `sequence`.
fn mega_leave_lines(sequence: u8) -> String {
    format!(
        "> {}\n< {}\n",
        hexadecimal(&message(sequence, &[0x11, 0x01, 0x01])),
        hexadecimal(&message(sequence, &[0x11, 0x00]))
    )
}

#[test]
fn a_mega_2560_flash_out_of_erase_order_is_refused_before_any_byte() {
    // Two images with a gap: the first would be written, then the second refused.
    let (manifest, mut source) = mega_two_images(0x200, "");
    let text = transcript("connect-mega2560") + &mega_leave_lines(5);
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(&manifest, &text, &mut source, &mut recorder);
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::PageOutOfEraseOrder {
            what: "image \"image at 0x200\"".into(),
            offset: 0x200,
            erases_next: 0x100,
        }
    );
    assert!(error.to_string().contains("nothing was written"), "{error}");
    // Connected, then left the bootloader: no page was programmed.
    provider.finish().unwrap();
    assert_eq!(stages(&recorder), [Stage::Connecting, Stage::Resetting]);

    // An erase entry from 0x0 moves the bootloader's erase address past the image at 0x0: the
    // sixteen pages would be blanked, then the image refused.
    let (manifest, mut source) =
        mega_two_images(0x100, "[[board.erase]]\noffset = 0\nsize = 0x1000\n");
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(&manifest, &text, &mut source, &mut recorder);
    assert_eq!(
        result.unwrap_err(),
        Error::PageOutOfEraseOrder {
            what: "image \"image at 0x0\"".into(),
            offset: 0,
            erases_next: 0x1000,
        }
    );
    provider.finish().unwrap();
    assert_eq!(stages(&recorder), [Stage::Connecting, Stage::Resetting]);

    // An erase entry that does not start at 0x0 is refused as well, before the images.
    let (manifest, mut source) =
        mega_two_images(0x100, "[[board.erase]]\noffset = 0x1000\nsize = 0x100\n");
    let (result, provider) = run_flash(&manifest, &text, &mut source, &mut Recorder::default());
    assert_eq!(
        result.unwrap_err(),
        Error::PageOutOfEraseOrder {
            what: "erase region at 0x1000".into(),
            offset: 0x1000,
            erases_next: 0,
        }
    );
    provider.finish().unwrap();
}

#[test]
fn a_mega_2560_writes_images_anywhere_after_erase_all() {
    // --erase-all programs every page up to 0x3C000 with 0xFF: then the images may leave gaps.
    let (manifest, mut source) = mega_two_images(0x200, "");
    let mut text = transcript("connect-mega2560");
    let mut sequence = 5_u8;
    let pages = (0..0x3_C000).step_by(0x100).chain([0, 0x200]);
    for offset in pages {
        text.push_str(&mega_program_lines(sequence, offset));
        sequence = sequence.wrapping_add(2);
    }
    text.push_str(&mega_leave_lines(sequence));
    let options = FlashOptions {
        erase_all: true,
        verify: false,
        ..FlashOptions::default()
    };
    let (result, provider) = run_flash_with(
        &manifest,
        &text,
        &mut source,
        &mut Recorder::default(),
        &options,
    );
    let report = result.unwrap();
    assert_eq!(
        images(&report),
        [("image at 0x0", 0, 0x100), ("image at 0x200", 0x200, 0x10)]
    );
    provider.finish().unwrap();
}
