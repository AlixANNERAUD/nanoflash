//! Transcript tests of the flash and detect orchestration.

use std::collections::BTreeMap;
use std::path::Path;

use nanoflash::Error;
use nanoflash::backend::{BackendOptions, Progress, ProgressEvent, Stage};
use nanoflash::flash::{FlashOptions, Hooks, detect, flash};
use nanoflash::manifest::{EraseTarget, Manifest, Protocol};
use nanoflash::testing::{MemorySource, ReplayProvider, Transcript, replay};
use pollster::block_on;

const MEBIBYTE: u64 = 1024 * 1024;

fn fixture(relative: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn transcript(name: &str) -> String {
    String::from_utf8(fixture(&format!("transcripts/espressif/{name}.transcript"))).unwrap()
}

fn manifest() -> Manifest {
    Manifest::from_json(core::str::from_utf8(&fixture("boards/esp32-s3-usb.json")).unwrap())
        .unwrap()
}

/// The layout and the images of the fixture board, under the paths its manifest uses.
fn fixture_source() -> MemorySource {
    let mut files = BTreeMap::new();
    for path in [
        "../layouts/esp32-s3-8mb.toml",
        "../images/bootloader.bin",
        "../images/application.bin",
    ] {
        files.insert(path.to_string(), fixture(&path[3..]));
    }
    MemorySource { files }
}

/// Records the stages, without repeating a stage that goes on, and the messages.
#[derive(Default)]
struct Recorder {
    stages: Vec<Stage>,
    events: Vec<ProgressEvent>,
    messages: Vec<String>,
}

impl Progress for Recorder {
    fn report(&mut self, event: ProgressEvent) {
        if self.stages.last() != Some(&event.stage) {
            self.stages.push(event.stage);
        }
        self.events.push(event);
    }

    fn message(&mut self, text: &str) {
        self.messages.push(text.to_string());
    }
}

/// Flashes `manifest` over a replay of `text`.
fn run_flash(
    manifest: &Manifest,
    text: &str,
    source: &mut MemorySource,
    options: &FlashOptions,
    progress: &mut Recorder,
) -> (Result<nanoflash::flash::FlashReport, Error>, ReplayProvider) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let hooks = Hooks {
        source,
        verifier: &mut (),
        progress,
    };
    let result = block_on(flash(manifest, port, &mut provider, hooks, options));
    (result, provider)
}

fn run_detect(
    text: &str,
    protocol: Option<Protocol>,
) -> (
    Result<nanoflash::backend::ChipInformation, Error>,
    ReplayProvider,
) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let result = block_on(detect(
        port,
        &mut provider,
        protocol,
        &BackendOptions::default(),
        &mut (),
    ));
    (result, provider)
}

#[test]
fn flashes_the_fixture_board() {
    let mut progress = Recorder::default();
    let (result, provider) = run_flash(
        &manifest(),
        &transcript("flash-esp32s3-usb"),
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut progress,
    );
    let report = result.unwrap();
    assert_eq!(report.board, "esp32-s3-devkitc");
    assert_eq!(report.chip.chip, "esp32-s3");
    assert_eq!(report.chip.flash_size, Some(8 * MEBIBYTE));
    let images: Vec<(&str, u64, u64)> = report
        .images
        .iter()
        .map(|image| (image.label.as_str(), image.offset, image.size))
        .collect();
    assert_eq!(
        images,
        [
            ("bootloader", 0x0, 4096),
            ("partition-table", 0x8000, 0xC00),
            ("application", 0x20000, 8192),
        ]
    );
    assert!(report.verified);
    assert!(report.warnings.is_empty());
    provider.finish().unwrap();
    assert_eq!(
        progress.stages,
        [
            Stage::Connecting,
            Stage::Writing,
            Stage::Verifying,
            Stage::Resetting
        ]
    );
}

#[test]
fn no_verify_skips_the_digests() {
    let text: String = transcript("flash-esp32s3-usb")
        .lines()
        .filter(|line| !line.contains(" 13 "))
        .map(|line| format!("{line}\n"))
        .collect();
    let mut progress = Recorder::default();
    let options = FlashOptions {
        verify: false,
        ..FlashOptions::default()
    };
    let (result, provider) = run_flash(
        &manifest(),
        &text,
        &mut fixture_source(),
        &options,
        &mut progress,
    );
    assert!(!result.unwrap().verified);
    provider.finish().unwrap();
    assert!(!progress.stages.contains(&Stage::Verifying));
}

#[test]
fn a_failed_verification_names_the_image() {
    // The application, checked last, reads back as 32 zeros.
    let text = transcript("flash-esp32s3-usb").replace(
        "37 30 30 37 64 64 39 63 30 36 31 33 61 37 31 62 35 38 33 65 66 37 61 62 35 65 62 65 63 38 31 31",
        &["30"; 32].join(" "),
    );
    let mut progress = Recorder::default();
    let (result, provider) = run_flash(
        &manifest(),
        &text,
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut progress,
    );
    let error = result.unwrap_err();
    assert!(
        matches!(
            &error,
            Error::VerificationFailed { label, offset: 0x20000, .. } if label == "application"
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("\"application\""), "{error}");
    // The board is still reset into its application.
    provider.finish().unwrap();
    assert_eq!(progress.stages.last(), Some(&Stage::Resetting));
}

#[test]
fn layout_larger_than_flash_stops_before_writing() {
    let mut progress = Recorder::default();
    let (result, provider) = run_flash(
        &manifest(),
        &transcript("flash-too-small"),
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut progress,
    );
    assert_eq!(
        result.unwrap_err(),
        Error::FlashTooSmall {
            layout: 8 * MEBIBYTE,
            detected: 4 * MEBIBYTE,
        }
    );
    provider.finish().unwrap();
    assert_eq!(progress.stages, [Stage::Connecting, Stage::Resetting]);
}

#[test]
fn a_failed_reset_after_an_error_is_reported() {
    // The transcript ends before the reset into the application, which then fails.
    let full = transcript("flash-too-small");
    let lines: Vec<&str> = full.lines().collect();
    let text: String = lines[..lines.len() - 5]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
    let mut progress = Recorder::default();
    let (result, _provider) = run_flash(
        &manifest(),
        &text,
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut progress,
    );
    assert!(
        matches!(result, Err(Error::FlashTooSmall { .. })),
        "{result:?}"
    );
    assert!(
        progress.messages.iter().any(|message| message
            .starts_with("could not reset into the application: ")
            && message.ends_with("; press the board's reset button")),
        "{:?}",
        progress.messages
    );
}

#[test]
fn erases_everything_first_when_asked() {
    // The whole 8 MiB of the layout: FLASH_BEGIN with 0x800000 bytes at 0x0, then the flash.
    let text = transcript("flash-esp32s3-usb").replacen(
        "> c0 00 10 ",
        "> c0 00 02 14 00 00 00 00 00 00 00 80 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 c0\n\
         < c0 01 02 04 00 00 00 00 00 00 00 00 00 c0\n> c0 00 10 ",
        1,
    );
    let mut progress = Recorder::default();
    let options = FlashOptions {
        erase_all: true,
        ..FlashOptions::default()
    };
    let (result, provider) = run_flash(
        &manifest(),
        &text,
        &mut fixture_source(),
        &options,
        &mut progress,
    );
    result.unwrap();
    provider.finish().unwrap();
    assert_eq!(
        progress.stages,
        [
            Stage::Connecting,
            Stage::Erasing,
            Stage::Writing,
            Stage::Verifying,
            Stage::Resetting
        ]
    );
    let erasing: Vec<&ProgressEvent> = progress
        .events
        .iter()
        .filter(|event| event.stage == Stage::Erasing)
        .collect();
    assert_eq!(erasing.len(), 2);
    assert_eq!(
        (erasing[0].offset, erasing[0].done, erasing[0].total),
        (0, 0, 8 * MEBIBYTE)
    );
    assert_eq!(erasing[1].done, 8 * MEBIBYTE);
}

#[test]
fn missing_image_fails_before_connecting() {
    let mut source = fixture_source();
    source.files.remove("../images/application.bin");
    let (result, provider) = run_flash(
        &manifest(),
        "! identity 303a:1001\n",
        &mut source,
        &FlashOptions::default(),
        &mut Recorder::default(),
    );
    match result {
        Err(Error::ImageSource { path, .. }) => assert_eq!(path, "../images/application.bin"),
        other => panic!("{other:?}"),
    }
    provider.finish().unwrap();
}

#[test]
fn detects_with_the_identity_table() {
    let (result, provider) = run_detect(&transcript("detect-esp32s3-usb"), None);
    let chip = result.unwrap();
    assert_eq!(chip.chip, "esp32-s3");
    assert_eq!(chip.protocol, Protocol::Espressif);
    provider.finish().unwrap();
}

#[test]
fn detect_without_identity_needs_a_protocol() {
    let (result, provider) = run_detect("# no identity\n", None);
    assert_eq!(
        result.unwrap_err(),
        Error::ProtocolRequired { identity: None }
    );
    provider.finish().unwrap();
}

#[test]
fn disabled_protocol() {
    let manifest = Manifest::from_toml(
        r#"
[[board]]
identifier = "samd21"
protocol = "samba"
chip = "samd21"

[[board.image]]
offset = 0x2000
path = "firmware.bin"
"#,
    )
    .unwrap();
    let mut source = MemorySource::default();
    source.files.insert("firmware.bin".to_string(), vec![0; 16]);
    let (result, provider) = run_flash(
        &manifest,
        "# a serial bridge\n",
        &mut source,
        &FlashOptions::default(),
        &mut Recorder::default(),
    );
    assert_eq!(
        result.unwrap_err(),
        Error::ProtocolNotEnabled {
            protocol: Protocol::Samba
        }
    );
    provider.finish().unwrap();
}

#[test]
fn erase_all_erases_the_flash_of_the_chip() {
    // The chip reports 16 MiB of flash, more than the 8 MiB of the layout: all of it is erased.
    let text = transcript("flash-esp32s3-usb")
        .replace(
            "< c0 01 0a 04 00 c8 40 17 00 00 00 00 00 c0",
            "< c0 01 0a 04 00 c8 40 18 00 00 00 00 00 c0",
        )
        .replace(
            "> c0 00 0b 18 00 00 00 00 00 00 00 00 00 00 00 80 00",
            "> c0 00 0b 18 00 00 00 00 00 00 00 00 00 00 00 00 01",
        )
        .replacen(
            "> c0 00 10 ",
            "> c0 00 02 14 00 00 00 00 00 00 00 00 01 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 c0\n\
             < c0 01 02 04 00 00 00 00 00 00 00 00 00 c0\n> c0 00 10 ",
            1,
        );
    let mut progress = Recorder::default();
    let options = FlashOptions {
        erase_all: true,
        ..FlashOptions::default()
    };
    let (result, provider) = run_flash(
        &manifest(),
        &text,
        &mut fixture_source(),
        &options,
        &mut progress,
    );
    assert_eq!(result.unwrap().chip.flash_size, Some(16 * MEBIBYTE));
    provider.finish().unwrap();
    let erasing = progress
        .events
        .iter()
        .find(|event| event.stage == Stage::Erasing)
        .unwrap();
    assert_eq!((erasing.offset, erasing.total), (0, 16 * MEBIBYTE));
}

#[test]
fn a_misaligned_erase_entry_fails_before_connecting() {
    let mut manifest = manifest();
    manifest.boards[0].erase.push(EraseTarget::Region {
        offset: 0x9800,
        size: 0x100,
    });
    let mut progress = Recorder::default();
    let (result, provider) = run_flash(
        &manifest,
        "! identity 303a:1001\n",
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut progress,
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::MisalignedErase {
            entry: "region at 0x9800".into(),
            offset: 0x9800,
            size: 0x100,
            required: 0x1000,
        }
    );
    assert!(error.to_string().contains("region at 0x9800"), "{error}");
    provider.finish().unwrap();
    assert!(progress.stages.is_empty(), "{:?}", progress.stages);
}

/// An esp-web-tools manifest whose builds write `bootloader.bin` at the given offsets.
fn esp_web_tools(builds: &[(&str, u64)]) -> Manifest {
    let builds: Vec<String> = builds
        .iter()
        .map(|(chip, offset)| {
            format!(
                r#"{{"chipFamily":"{chip}","parts":[{{"path":"../images/bootloader.bin","offset":{offset}}}]}}"#
            )
        })
        .collect();
    Manifest::from_json(&format!(r#"{{"builds":[{}]}}"#, builds.join(","))).unwrap()
}

#[test]
fn a_part_beyond_four_gibibytes_is_refused_before_erasing() {
    // Two builds: the board is only known, and its images read, once the chip is detected.
    let manifest = esp_web_tools(&[("ESP32", 0x1000), ("ESP32-S3", 0x1_0000_0000)]);
    let mut progress = Recorder::default();
    let options = FlashOptions {
        erase_all: true,
        ..FlashOptions::default()
    };
    let (result, provider) = run_flash(
        &manifest,
        &transcript("detect-esp32s3-usb"),
        &mut fixture_source(),
        &options,
        &mut progress,
    );
    assert_eq!(
        result.unwrap_err(),
        Error::OutsideAddressRange {
            what: "image \"image at 0x100000000\"".into(),
            end: 0x1_0000_1000,
            limit: 0x1_0000_0000,
            protocol: Protocol::Espressif,
        }
    );
    // Connected, then reset into the application: nothing was erased.
    provider.finish().unwrap();
    assert_eq!(progress.stages, [Stage::Connecting, Stage::Resetting]);
}

#[test]
fn a_part_beyond_the_flash_of_the_chip_is_refused_before_erasing() {
    let manifest = esp_web_tools(&[("ESP32-S3", 0x40_0000)]);
    let mut progress = Recorder::default();
    let options = FlashOptions {
        erase_all: true,
        ..FlashOptions::default()
    };
    // The chip reports 4 MiB of flash.
    let (result, provider) = run_flash(
        &manifest,
        &transcript("flash-too-small"),
        &mut fixture_source(),
        &options,
        &mut progress,
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::OutsideDetectedFlash {
            what: "image \"image at 0x400000\"".into(),
            end: 0x40_1000,
            flash_size: 4 * MEBIBYTE,
        }
    );
    assert!(error.to_string().contains("nothing was erased"), "{error}");
    provider.finish().unwrap();
    assert_eq!(progress.stages, [Stage::Connecting, Stage::Resetting]);
}

#[test]
fn a_disconnection_says_what_was_being_done() {
    let mut progress = Recorder::default();
    let (result, _provider) = run_flash(
        &manifest(),
        &transcript("flash-disconnect"),
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut progress,
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::Transport {
            error: nanoflash::transport::TransportError::Disconnected,
            during: Some("writing \"bootloader\" at 0x0".into()),
        }
    );
    assert_eq!(
        error.to_string(),
        "the device disconnected while writing \"bootloader\" at 0x0: check the cable and the port, then try again"
    );
}
