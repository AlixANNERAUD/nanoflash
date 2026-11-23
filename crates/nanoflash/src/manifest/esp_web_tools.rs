//! Conversion of esp-web-tools manifests: one board per build, images from `parts`.
//!
//! Only what affects flashing is read; every other key (`improv`, `funding_url`, ...) is ignored.
//! `ESP8266` builds are dropped, since nanoflash flashes the ESP32 family only.

use super::{Board, Image, ImageTarget, Manifest, ManifestError, Protocol, ResetSequence};
use crate::quantity;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde::Deserialize;

#[derive(Deserialize)]
struct EspWebTools {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    new_install_prompt_erase: bool,
    builds: Vec<Build>,
}

#[derive(Deserialize)]
struct Build {
    #[serde(rename = "chipFamily")]
    chip_family: String,
    #[serde(default, rename = "serialType")]
    serial_type: Option<String>,
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    path: String,
    #[serde(deserialize_with = "quantity::deserialize")]
    offset: u64,
}

/// Convert the text of an esp-web-tools manifest.
pub(super) fn convert(text: &str) -> Result<Manifest, ManifestError> {
    let parsed: EspWebTools = serde_json::from_str(text).map_err(|error| ManifestError::Parse {
        message: error.to_string(),
    })?;
    let total = parsed.builds.len();
    let builds: Vec<Build> = parsed
        .builds
        .into_iter()
        .filter(|build| !build.chip_family.eq_ignore_ascii_case("esp8266"))
        .collect();
    if builds.is_empty() && total > 0 {
        return Err(ManifestError::OnlyEsp8266);
    }
    let mut boards: Vec<Board> = Vec::new();
    for build in builds {
        let chip = build.chip_family.to_ascii_lowercase();
        let repeats = boards.iter().filter(|board| board.chip == chip).count();
        let identifier = if repeats == 0 {
            chip.clone()
        } else {
            alloc::format!("{chip}-{}", repeats + 1)
        };
        let name = match &parsed.name {
            Some(name) => alloc::format!("{name} ({})", build.chip_family),
            None => build.chip_family.clone(),
        };
        // An absent or unknown `serialType` leaves the reset to the backend, which decides
        // from the port.
        let reset = match build.serial_type.as_deref() {
            Some("uart") => Some(ResetSequence::EspressifClassic),
            Some("cdc") => Some(ResetSequence::EspressifUsbSerialJtag),
            _ => None,
        };
        boards.push(Board {
            identifier,
            name: Some(name),
            protocol: Protocol::Espressif,
            chip,
            layout: None,
            reset,
            application_devices: Vec::new(),
            bootloader_devices: Vec::new(),
            images: build
                .parts
                .into_iter()
                .map(|part| Image {
                    target: ImageTarget::Offset(part.offset),
                    path: part.path,
                    sha256: None,
                })
                .collect(),
            erase: Vec::new(),
        });
    }
    Manifest::new(boards, parsed.new_install_prompt_erase)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/boards/esp-web-tools.json");

    #[test]
    fn converts_esp_web_tools() {
        let manifest = Manifest::parse(FIXTURE).unwrap();
        assert!(manifest.prompt_erase);
        let identifiers: Vec<&str> = manifest
            .boards
            .iter()
            .map(|b| b.identifier.as_str())
            .collect();
        assert_eq!(identifiers, ["esp32", "esp32-s3"]);
        let esp32 = &manifest.boards[0];
        assert_eq!(esp32.name.as_deref(), Some("Demo (ESP32)"));
        assert_eq!(esp32.protocol, Protocol::Espressif);
        assert_eq!(esp32.layout, None);
        assert_eq!(esp32.reset, Some(ResetSequence::EspressifClassic));
        assert_eq!(esp32.chip, "esp32");
        assert_eq!(
            esp32.images,
            [
                Image {
                    target: ImageTarget::Offset(0x1000),
                    path: "../images/bootloader.bin".into(),
                    sha256: None
                },
                Image {
                    target: ImageTarget::Offset(0x10000),
                    path: "../images/application.bin".into(),
                    sha256: None
                },
            ]
        );
        assert_eq!(manifest.boards[1].reset, None);
        assert_eq!(manifest.boards[1].name.as_deref(), Some("Demo (ESP32-S3)"));
        assert!(manifest.boards[1].application_devices.is_empty());
    }

    #[test]
    fn cdc_gives_usb_serial_jtag_and_repeats_are_numbered() {
        let text = r#"{"builds":[
            {"chipFamily":"ESP32-C3","serialType":"cdc","parts":[]},
            {"chipFamily":"ESP32-C3","parts":[]},
            {"chipFamily":"ESP32-C3","serialType":"other","parts":[]}]}"#;
        let manifest = Manifest::from_json(text).unwrap();
        let identifiers: Vec<&str> = manifest
            .boards
            .iter()
            .map(|b| b.identifier.as_str())
            .collect();
        assert_eq!(identifiers, ["esp32-c3", "esp32-c3-2", "esp32-c3-3"]);
        assert_eq!(
            manifest.boards[0].reset,
            Some(ResetSequence::EspressifUsbSerialJtag)
        );
        assert_eq!(manifest.boards[0].name, Some("ESP32-C3".into()));
        assert_eq!(manifest.boards[1].reset, None);
        assert!(!manifest.prompt_erase);
    }

    #[test]
    fn esp8266_only_is_refused() {
        let text = r#"{"name":"Old","builds":[{"chipFamily":"ESP8266","parts":[]}]}"#;
        let error = Manifest::parse(text).unwrap_err();
        assert_eq!(error, ManifestError::OnlyEsp8266);
        assert!(error.to_string().contains("ESP8266"));
    }

    #[test]
    fn no_builds_is_an_empty_manifest() {
        assert_eq!(
            Manifest::from_json(r#"{"builds":[]}"#),
            Err(ManifestError::NoBoards)
        );
    }
}
