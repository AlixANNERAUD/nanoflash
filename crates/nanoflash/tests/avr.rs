//! Transcript tests of the AVR backend.

use std::path::Path;

use nanoflash::avr::{AvrBackend, AvrOptions};
use nanoflash::backend::{ChipInformation, ProgressEvent};
use nanoflash::manifest::{Protocol, ResetSequence, ResetStep};
use nanoflash::testing::{ReplayProvider, Transcript, replay};
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
            writable_to: Some(0x3_E000),
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
