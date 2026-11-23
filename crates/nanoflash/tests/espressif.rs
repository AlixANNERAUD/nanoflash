//! Transcript tests of the Espressif backend.

use std::fmt::Write as _;
use std::path::Path;

use nanoflash::backend::{ChipInformation, ProgressEvent};
use nanoflash::espressif::{Chip, EspressifBackend, EspressifOptions};
use nanoflash::manifest::{Protocol, ResetSequence};
use nanoflash::testing::{ReplayProvider, Transcript, replay};
use nanoflash::transport::UsbIdentity;
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
