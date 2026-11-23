//! The board manifest: which boards a project supports, how to reset them and what to write.
//!
//! A manifest is a TOML or JSON file with the same keys (kebab-case). An esp-web-tools manifest
//! (JSON with `builds`) is detected and converted, see [`esp_web_tools`].

#[cfg(feature = "json")]
mod esp_web_tools;

use crate::quantity;
use crate::transport::{DeviceFilter, Duration};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// The wire protocol a board speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// The Espressif ROM and stub loader.
    Espressif,
    /// The SAM-BA monitor.
    Samba,
    /// STK500v1 (Optiboot and the older Arduino bootloaders); `arduino` in a manifest also
    /// names it, as Arduino's `boards.txt` does.
    #[serde(alias = "arduino")]
    Stk500v1,
    /// STK500v2 (the Mega 2560's wiring bootloader); `wiring` in a manifest also names it, as
    /// Arduino's `boards.txt` does.
    #[serde(alias = "wiring")]
    Stk500v2,
    /// AVR109 (Caterina).
    Avr109,
}

impl fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Protocol::Espressif => "espressif",
            Protocol::Samba => "samba",
            Protocol::Stk500v1 => "stk500v1",
            Protocol::Stk500v2 => "stk500v2",
            Protocol::Avr109 => "avr109",
        })
    }
}

/// How to put a board in its bootloader and back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResetSequence {
    /// DTR and RTS through a UART bridge.
    EspressifClassic,
    /// DTR and RTS of the USB-Serial-JTAG peripheral.
    EspressifUsbSerialJtag,
    /// Open the port at 1200 baud and close it.
    #[serde(rename = "touch-1200")]
    Touch1200,
    /// Open the port at 1200 baud and close it, then wait for a UF2 drive.
    #[serde(rename = "uf2-touch-1200")]
    Uf2Touch1200,
    /// No reset: the board is already in its bootloader.
    None,
    /// Explicit steps.
    Custom(Vec<ResetStep>),
}

impl ResetSequence {
    /// The name of the sequence, as a manifest writes it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            ResetSequence::EspressifClassic => "espressif-classic",
            ResetSequence::EspressifUsbSerialJtag => "espressif-usb-serial-jtag",
            ResetSequence::Touch1200 => "touch-1200",
            ResetSequence::Uf2Touch1200 => "uf2-touch-1200",
            ResetSequence::None => "none",
            ResetSequence::Custom(_) => "custom",
        }
    }
}

/// The name of the sequence, as a manifest writes it, such as `touch-1200` or `custom`.
impl fmt::Display for ResetSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One step of a custom reset: set the lines, then wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetStep {
    /// Data terminal ready.
    pub dtr: bool,
    /// Request to send.
    pub rts: bool,
    /// How long to wait after setting the lines, at most 10 s.
    #[serde(
        default,
        deserialize_with = "deserialize_wait",
        serialize_with = "serialize_duration"
    )]
    pub wait: Duration,
}

fn serialize_duration<S: Serializer>(
    duration: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&format_args!("{} ms", duration.as_millis()))
}

/// The longest a custom reset step may wait: resets take milliseconds, and the wait blocks.
pub const MAXIMUM_RESET_WAIT: Duration = Duration::from_secs(10);

fn deserialize_wait<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let wait = deserialize_duration(deserializer)?;
    if wait > MAXIMUM_RESET_WAIT {
        return Err(de::Error::custom(alloc::format!(
            "a reset step waits {} ms, longer than the {} ms a step may wait: shorten the wait",
            wait.as_millis(),
            MAXIMUM_RESET_WAIT.as_millis()
        )));
    }
    Ok(wait)
}

fn deserialize_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    struct DurationVisitor;

    impl Visitor<'_> for DurationVisitor {
        type Value = Duration;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a number of milliseconds, or text such as \"100 ms\" or \"2 s\"")
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Duration, E> {
            Ok(Duration::from_millis(value))
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Duration, E> {
            u64::try_from(value)
                .map(Duration::from_millis)
                .map_err(|_| E::custom(alloc::format!("negative duration {value}")))
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Duration, E> {
            let text = value.trim_matches(' ');
            let split = text
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(text.len());
            let (digits, unit) = text.split_at(split);
            let number: Option<u64> = digits.parse().ok();
            let duration = match (number, unit.trim_start_matches(' ')) {
                (Some(number), "ms") => Some(Duration::from_millis(number)),
                (Some(number), "s") => number.checked_mul(1000).map(Duration::from_millis),
                _ => None,
            };
            duration.ok_or_else(|| {
                E::custom(alloc::format!(
                    "invalid duration \"{value}\": expected milliseconds (100), \"100 ms\" or \"2 s\""
                ))
            })
        }
    }

    deserializer.deserialize_any(DurationVisitor)
}

/// Where an image goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageTarget {
    /// A partition of the board's layout.
    Partition(String),
    /// A flash offset.
    Offset(u64),
}

/// A file to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Image {
    /// Where it goes.
    pub target: ImageTarget,
    /// Path relative to the manifest, or a URL.
    pub path: String,
    /// The expected SHA-256 of the file.
    #[serde(serialize_with = "serialize_sha256")]
    pub sha256: Option<[u8; 32]>,
}

fn serialize_sha256<S: Serializer>(
    digest: &Option<[u8; 32]>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match digest {
        Some(digest) => {
            let mut text = String::with_capacity(64);
            for byte in digest {
                text.push_str(&alloc::format!("{byte:02x}"));
            }
            serializer.serialize_some(&text)
        }
        None => serializer.serialize_none(),
    }
}

/// A region to erase before writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", try_from = "RawErase")]
pub enum EraseTarget {
    /// A partition of the board's layout.
    Partition(String),
    /// A flash region.
    Region {
        /// First byte.
        offset: u64,
        /// Length in bytes.
        size: u64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawErase {
    partition: Option<String>,
    #[serde(default, deserialize_with = "quantity::deserialize_optional")]
    offset: Option<u64>,
    #[serde(default, deserialize_with = "quantity::deserialize_optional")]
    size: Option<u64>,
}

impl TryFrom<RawErase> for EraseTarget {
    type Error = String;

    fn try_from(raw: RawErase) -> Result<Self, String> {
        match (raw.partition, raw.offset, raw.size) {
            (Some(partition), None, None) => Ok(EraseTarget::Partition(partition)),
            (None, Some(_), Some(0)) => {
                Err("an erase entry needs a \"size\" of at least one byte".to_string())
            }
            (None, Some(offset), Some(size)) => Ok(EraseTarget::Region { offset, size }),
            (None, Some(_), None) => {
                Err("an erase entry with an \"offset\" needs a \"size\"".to_string())
            }
            (None, None, Some(_)) => {
                Err("an erase entry with a \"size\" needs an \"offset\"".to_string())
            }
            (None, None, None) => Err(
                "an erase entry needs a \"partition\", or an \"offset\" and a \"size\"".to_string(),
            ),
            (Some(_), _, _) => Err(
                "an erase entry takes a \"partition\", or an \"offset\" and a \"size\", not both"
                    .to_string(),
            ),
        }
    }
}

/// A board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", try_from = "RawBoard")]
pub struct Board {
    /// Unique identifier, selected with `--board-identifier`.
    pub identifier: String,
    /// Human readable name.
    pub name: Option<String>,
    /// The wire protocol.
    pub protocol: Protocol,
    /// The chip, as the protocol reports or names it.
    pub chip: String,
    /// Path of the layout file, relative to the manifest.
    pub layout: Option<String>,
    /// How to reset the board; the backend decides when absent.
    pub reset: Option<ResetSequence>,
    /// USB identities of the board running its application.
    pub application_devices: Vec<DeviceFilter>,
    /// USB identities of the board in its bootloader.
    pub bootloader_devices: Vec<DeviceFilter>,
    /// The files to write.
    pub images: Vec<Image>,
    /// The regions to erase first.
    pub erase: Vec<EraseTarget>,
}

impl Board {
    /// Whether `chip` names this board's chip: ASCII case-insensitive, ignoring `-` and `_`.
    pub fn matches_chip(&self, chip: &str) -> bool {
        let significant = |text: &str| -> Vec<u8> {
            text.bytes()
                .filter(|byte| !matches!(byte, b'-' | b'_'))
                .map(|byte| byte.to_ascii_lowercase())
                .collect()
        };
        significant(&self.chip) == significant(chip)
    }
}

/// A manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", try_from = "RawManifest")]
pub struct Manifest {
    /// The boards, in file order.
    pub boards: Vec<Board>,
    /// Whether to erase the whole flash first, unless told otherwise.
    pub prompt_erase: bool,
}

/// A manifest that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ManifestError {
    /// The text is not a valid manifest; the message has the line and column.
    Parse {
        /// What the parser said.
        message: String,
    },
    /// The manifest lists no board.
    NoBoards,
    /// Two boards share an identifier.
    DuplicateBoard {
        /// The shared identifier.
        identifier: String,
    },
    /// An image has no usable target or digest.
    InvalidImage {
        /// The board.
        board: String,
        /// The path of the image.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// An esp-web-tools manifest with ESP8266 builds only.
    OnlyEsp8266,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Parse { message } => formatter.write_str(message),
            ManifestError::NoBoards => formatter.write_str(
                "the manifest lists no board: add a [[board]] table (or a \"board\" array in JSON)",
            ),
            ManifestError::DuplicateBoard { identifier } => write!(
                formatter,
                "the manifest lists the board \"{identifier}\" more than once: board identifiers must be unique"
            ),
            ManifestError::InvalidImage {
                board,
                path,
                message,
            } => write!(
                formatter,
                "the image \"{path}\" of the board \"{board}\" is invalid: {message}"
            ),
            ManifestError::OnlyEsp8266 => formatter
                .write_str("ESP8266 is not supported: nanoflash flashes the ESP32 family only"),
        }
    }
}

impl core::error::Error for ManifestError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawManifest {
    #[serde(default)]
    prompt_erase: bool,
    #[serde(default, rename = "board")]
    boards: Vec<RawBoard>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawBoard {
    identifier: String,
    #[serde(default)]
    name: Option<String>,
    protocol: Protocol,
    chip: String,
    #[serde(default)]
    layout: Option<String>,
    #[serde(default)]
    reset: Option<ResetSequence>,
    #[serde(default)]
    application_devices: Vec<DeviceFilter>,
    #[serde(default)]
    bootloader_devices: Vec<DeviceFilter>,
    #[serde(default, rename = "image")]
    images: Vec<RawImage>,
    #[serde(default)]
    erase: Vec<EraseTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImage {
    #[serde(default)]
    partition: Option<String>,
    #[serde(default, deserialize_with = "quantity::deserialize_optional")]
    offset: Option<u64>,
    path: String,
    #[serde(default)]
    sha256: Option<String>,
}

fn parse_sha256(text: &str) -> Result<[u8; 32], String> {
    let invalid = || {
        alloc::format!(
            "\"sha256\" must be 64 hexadecimal digits, found {} characters",
            text.chars().count()
        )
    };
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let mut digest = [0u8; 32];
    for (slot, pair) in digest.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = core::str::from_utf8(pair).map_err(|_| invalid())?;
        *slot = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
    }
    Ok(digest)
}

fn convert_image(board: &str, raw: RawImage) -> Result<Image, ManifestError> {
    let invalid = |message: String| ManifestError::InvalidImage {
        board: board.to_string(),
        path: raw.path.clone(),
        message,
    };
    let target = match (&raw.partition, raw.offset) {
        (Some(partition), None) => ImageTarget::Partition(partition.clone()),
        (None, Some(offset)) => ImageTarget::Offset(offset),
        (Some(_), Some(_)) => {
            return Err(invalid(
                "give either a \"partition\" or an \"offset\", not both".to_string(),
            ));
        }
        (None, None) => {
            return Err(invalid(
                "give a \"partition\" or an \"offset\" to say where the image goes".to_string(),
            ));
        }
    };
    let sha256 = match &raw.sha256 {
        Some(text) => Some(parse_sha256(text).map_err(invalid)?),
        None => None,
    };
    Ok(Image {
        target,
        path: raw.path.clone(),
        sha256,
    })
}

impl TryFrom<RawBoard> for Board {
    type Error = ManifestError;

    fn try_from(raw: RawBoard) -> Result<Self, ManifestError> {
        let images = raw
            .images
            .into_iter()
            .map(|image| convert_image(&raw.identifier, image))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Board {
            identifier: raw.identifier,
            name: raw.name,
            protocol: raw.protocol,
            chip: raw.chip,
            layout: raw.layout,
            reset: raw.reset,
            application_devices: raw.application_devices,
            bootloader_devices: raw.bootloader_devices,
            images,
            erase: raw.erase,
        })
    }
}

impl TryFrom<RawManifest> for Manifest {
    type Error = ManifestError;

    fn try_from(raw: RawManifest) -> Result<Self, ManifestError> {
        let boards = raw
            .boards
            .into_iter()
            .map(Board::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Manifest::new(boards, raw.prompt_erase)
    }
}

impl Manifest {
    /// Check that there is a board and that identifiers are unique.
    fn new(boards: Vec<Board>, prompt_erase: bool) -> Result<Manifest, ManifestError> {
        if boards.is_empty() {
            return Err(ManifestError::NoBoards);
        }
        for (index, board) in boards.iter().enumerate() {
            if boards
                .iter()
                .take(index)
                .any(|earlier| earlier.identifier == board.identifier)
            {
                return Err(ManifestError::DuplicateBoard {
                    identifier: board.identifier.clone(),
                });
            }
        }
        Ok(Manifest {
            boards,
            prompt_erase,
        })
    }

    /// Read a manifest from TOML.
    #[cfg(feature = "toml")]
    pub fn from_toml(text: &str) -> Result<Manifest, ManifestError> {
        let raw: RawManifest = toml::from_str(text).map_err(|error| ManifestError::Parse {
            message: error.to_string(),
        })?;
        Manifest::try_from(raw)
    }

    /// Read a manifest from JSON: ours, or an esp-web-tools manifest (detected by its `builds`).
    #[cfg(feature = "json")]
    pub fn from_json(text: &str) -> Result<Manifest, ManifestError> {
        let parse_error = |error: serde_json::Error| ManifestError::Parse {
            message: error.to_string(),
        };
        #[derive(Deserialize)]
        struct Probe {
            #[serde(default)]
            builds: Option<de::IgnoredAny>,
        }
        let probe: Probe = serde_json::from_str(text).map_err(parse_error)?;
        if probe.builds.is_some() {
            return esp_web_tools::convert(text);
        }
        let raw: RawManifest = serde_json::from_str(text).map_err(parse_error)?;
        Manifest::try_from(raw)
    }

    /// Read a manifest, as JSON when the first non-blank character is `{`, as TOML otherwise. A
    /// leading UTF-8 byte order mark, which some editors write, is skipped.
    #[cfg(any(feature = "toml", feature = "json"))]
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let json = text.trim_start().starts_with('{');
        #[cfg(feature = "json")]
        if json {
            return Manifest::from_json(text);
        }
        #[cfg(feature = "toml")]
        if !json {
            return Manifest::from_toml(text);
        }
        Err(ManifestError::Parse {
            message: alloc::format!(
                "cannot read a {} manifest: enable the \"{}\" feature",
                if json { "JSON" } else { "TOML" },
                if json { "json" } else { "toml" }
            ),
        })
    }
}

#[cfg(all(test, feature = "toml", feature = "json"))]
mod tests {
    use super::*;

    const TOML: &str = include_str!("../../tests/boards/esp32-s3-usb.toml");
    const JSON: &str = include_str!("../../tests/boards/esp32-s3-usb.json");

    #[test]
    fn reset_names_are_the_manifest_names() {
        for sequence in [
            ResetSequence::EspressifClassic,
            ResetSequence::EspressifUsbSerialJtag,
            ResetSequence::Touch1200,
            ResetSequence::Uf2Touch1200,
            ResetSequence::None,
            ResetSequence::Custom(Vec::new()),
        ] {
            let written = serde_json::to_value(&sequence).unwrap();
            let written = match &written {
                serde_json::Value::String(name) => name.as_str(),
                serde_json::Value::Object(map) => map.keys().next().unwrap().as_str(),
                other => panic!("{other:?}"),
            };
            assert_eq!(sequence.name(), written);
            assert_eq!(sequence.to_string(), written);
        }
    }

    fn board(extra: &str) -> alloc::string::String {
        alloc::format!(
            "[[board]]\nidentifier = \"b\"\nprotocol = \"espressif\"\nchip = \"esp32\"\n{extra}\n"
        )
    }

    #[test]
    fn parses_the_design_example() {
        let manifest = Manifest::parse(
            r#"
[[board]]
identifier = "arduino-nano-33-ble"
name = "Arduino Nano 33 BLE"
protocol = "samba"
chip = "nrf52840"
layout = "../layouts/nano33ble.toml"
reset = "touch-1200"
application-devices = [{ vendor = 0x2341, product = 0x805a }]
bootloader-devices = [{ vendor = 0x2341, product = 0x005a }]

[[board.image]]
partition = "application"
path = "firmware.bin"
sha256 = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
"#,
        )
        .unwrap();
        let board = &manifest.boards[0];
        assert_eq!(board.protocol, Protocol::Samba);
        assert_eq!(board.reset, Some(ResetSequence::Touch1200));
        assert_eq!(
            board.application_devices,
            [DeviceFilter {
                vendor: 0x2341,
                product: Some(0x805a),
                serial_number: None,
            }]
        );
        assert_eq!(board.bootloader_devices[0].product, Some(0x005a));
        assert_eq!(board.images[0].sha256.unwrap()[0], 0x9f);
        assert!(!manifest.prompt_erase);
    }

    #[test]
    fn toml_and_json_give_the_same_manifest() {
        let from_toml = Manifest::from_toml(TOML).unwrap();
        assert_eq!(from_toml, Manifest::from_json(JSON).unwrap());
        assert_eq!(from_toml, Manifest::parse(JSON).unwrap());
        let board = &from_toml.boards[0];
        assert_eq!(board.identifier, "esp32-s3-devkitc");
        assert_eq!(board.reset, Some(ResetSequence::EspressifUsbSerialJtag));
        assert_eq!(board.images.len(), 2);
        assert_eq!(
            board.images[0].target,
            ImageTarget::Partition("bootloader".into())
        );
        assert_eq!(board.images[1].sha256, None);
    }

    #[test]
    fn image_needs_exactly_one_target() {
        for image in [
            "partition = \"a\"\noffset = 4096\npath = \"x.bin\"",
            "path = \"x.bin\"",
        ] {
            let text = board(&alloc::format!("[[board.image]]\n{image}"));
            match Manifest::from_toml(&text).unwrap_err() {
                ManifestError::InvalidImage { board, path, .. } => {
                    assert_eq!((board.as_str(), path.as_str()), ("b", "x.bin"));
                }
                other => panic!("{other:?}"),
            }
        }
        let text = board("[[board.image]]\noffset = \"0x1000\"\npath = \"x.bin\"");
        assert_eq!(
            Manifest::from_toml(&text).unwrap().boards[0].images[0].target,
            ImageTarget::Offset(0x1000)
        );
    }

    #[test]
    fn sha256_must_be_64_hexadecimal_digits() {
        let short = "a".repeat(63);
        let bad = alloc::format!("{}g", "a".repeat(63));
        for digest in [short, bad] {
            let text = board(&alloc::format!(
                "[[board.image]]\noffset = 0\npath = \"x.bin\"\nsha256 = \"{digest}\""
            ));
            assert!(matches!(
                Manifest::from_toml(&text),
                Err(ManifestError::InvalidImage { .. })
            ));
        }
    }

    #[test]
    fn reset_sequences() {
        let reset = |value: &str| Manifest::from_toml(&board(&alloc::format!("reset = {value}")));
        assert_eq!(
            reset("\"espressif-classic\"").unwrap().boards[0].reset,
            Some(ResetSequence::EspressifClassic)
        );
        assert_eq!(
            reset("\"uf2-touch-1200\"").unwrap().boards[0].reset,
            Some(ResetSequence::Uf2Touch1200)
        );
        assert_eq!(
            reset("\"none\"").unwrap().boards[0].reset,
            Some(ResetSequence::None)
        );
        let custom = reset(
            "{ custom = [{ dtr = true, rts = false, wait = \"100 ms\" }, { dtr = false, rts = false, wait = 50 }] }",
        )
        .unwrap();
        assert_eq!(
            custom.boards[0].reset,
            Some(ResetSequence::Custom(alloc::vec![
                ResetStep {
                    dtr: true,
                    rts: false,
                    wait: Duration::from_millis(100)
                },
                ResetStep {
                    dtr: false,
                    rts: false,
                    wait: Duration::from_millis(50)
                },
            ]))
        );
        let seconds = reset("{ custom = [{ dtr = true, rts = true, wait = \"2 s\" }, { dtr = false, rts = false }] }")
            .unwrap();
        let Some(ResetSequence::Custom(steps)) = &seconds.boards[0].reset else {
            panic!()
        };
        assert_eq!(steps[0].wait, Duration::from_secs(2));
        assert_eq!(steps[1].wait, Duration::ZERO);
        let message = reset("\"reset-please\"").unwrap_err().to_string();
        assert!(message.contains("espressif-classic"), "{message}");
        assert!(message.contains("touch-1200"), "{message}");
        assert!(message.contains("custom"), "{message}");
    }

    #[test]
    fn reset_waits_are_capped_at_ten_seconds() {
        let wait = |value: &str| {
            Manifest::from_toml(&board(&alloc::format!(
                "reset = {{ custom = [{{ dtr = true, rts = false, wait = {value} }}] }}"
            )))
        };
        assert!(wait("\"10 s\"").is_ok());
        assert!(wait("10000").is_ok());
        for value in ["\"11 s\"", "10001", "\"18446744073709551 s\""] {
            let message = wait(value).unwrap_err().to_string();
            assert!(message.contains("10000 ms a step may wait"), "{message}");
        }
    }

    #[test]
    fn a_byte_order_mark_is_skipped() {
        let json = alloc::format!("\u{FEFF}{JSON}");
        assert_eq!(Manifest::parse(&json), Manifest::from_json(JSON));
        let toml = alloc::format!("\u{FEFF}{TOML}");
        assert_eq!(Manifest::parse(&toml), Manifest::from_toml(TOML));
    }

    #[test]
    fn device_filters_accept_hexadecimal_strings() {
        let with = |vendor: &str| {
            alloc::format!(
                r#"{{"board":[{{"identifier":"b","protocol":"espressif","chip":"esp32","application-devices":[{{"vendor":{vendor}}}]}}]}}"#
            )
        };
        let text = Manifest::from_json(&with("\"0x303a\"")).unwrap();
        let number = Manifest::from_json(&with("12346")).unwrap();
        assert_eq!(text, number);
        assert_eq!(
            text.boards[0].application_devices,
            [DeviceFilter {
                vendor: 0x303a,
                product: None,
                serial_number: None,
            }]
        );
        let message = Manifest::from_json(&with("\"0x10000\""))
            .unwrap_err()
            .to_string();
        assert!(message.contains("0x10000"), "{message}");
    }

    #[test]
    fn device_filters_may_name_a_serial_number() {
        let manifest = Manifest::from_toml(&board(
            "application-devices = [{ vendor = 0x303a, product = 0x1001, serial-number = \"A1\" }]",
        ))
        .unwrap();
        assert_eq!(
            manifest.boards[0].application_devices[0]
                .serial_number
                .as_deref(),
            Some("A1")
        );
        let text = serde_json::to_string(&manifest).unwrap();
        assert!(text.contains("\"serial-number\":\"A1\""), "{text}");
    }

    #[test]
    fn duplicate_identifiers_and_empty_manifests() {
        let text = alloc::format!("{}{}", board(""), board(""));
        assert_eq!(
            Manifest::from_toml(&text),
            Err(ManifestError::DuplicateBoard {
                identifier: "b".into()
            })
        );
        assert_eq!(
            Manifest::from_toml("prompt-erase = true"),
            Err(ManifestError::NoBoards)
        );
        assert_eq!(Manifest::from_json("{}"), Err(ManifestError::NoBoards));
    }

    #[test]
    fn rejects_unknown_keys() {
        let message = Manifest::from_toml(&board("colour = 1"))
            .unwrap_err()
            .to_string();
        assert!(message.contains("colour"), "{message}");
        assert!(message.contains("line"), "{message}");
        assert!(Manifest::from_toml("colour = 1").is_err());
    }

    #[test]
    fn erase_entries() {
        let text = board(
            "[[board.erase]]\npartition = \"nvs\"\n[[board.erase]]\noffset = \"4 KiB\"\nsize = 8192",
        );
        assert_eq!(
            Manifest::from_toml(&text).unwrap().boards[0].erase,
            [
                EraseTarget::Partition("nvs".into()),
                EraseTarget::Region {
                    offset: 4096,
                    size: 8192
                }
            ]
        );
        assert!(Manifest::from_toml(&board("[[board.erase]]\noffset = 0")).is_err());
        let message = Manifest::from_toml(&board("[[board.erase]]\noffset = 0\nsize = 0"))
            .unwrap_err()
            .to_string();
        assert!(message.contains("at least one byte"), "{message}");
    }

    #[test]
    fn chip_names_match_loosely() {
        let manifest = Manifest::from_toml(&board("").replace("esp32", "esp32-s3")).unwrap();
        let board = &manifest.boards[0];
        for chip in ["ESP32-S3", "esp32s3", "esp32_s3", "esp32-s3"] {
            assert!(board.matches_chip(chip), "{chip}");
        }
        assert!(!board.matches_chip("esp32"));
    }

    #[test]
    fn serialises_the_manifest() {
        let manifest = Manifest::from_toml(TOML).unwrap();
        let text = serde_json::to_string(&manifest).unwrap();
        assert!(text.contains("\"application-devices\""), "{text}");
        assert!(text.contains("d67c656e"), "{text}");
        let error = serde_json::to_string(&ManifestError::NoBoards).unwrap();
        assert_eq!(error, r#"{"kind":"no-boards"}"#);
    }

    #[test]
    fn protocol_names() {
        assert_eq!(Protocol::Espressif.to_string(), "espressif");
        assert_eq!(Protocol::Samba.to_string(), "samba");
    }

    #[test]
    fn avr_protocol_names() {
        for (protocol, name) in [
            (Protocol::Stk500v1, "stk500v1"),
            (Protocol::Stk500v2, "stk500v2"),
            (Protocol::Avr109, "avr109"),
        ] {
            assert_eq!(protocol.to_string(), name);
            assert_eq!(
                serde_json::to_value(protocol).unwrap(),
                serde_json::Value::String(name.into())
            );
            let parsed: Protocol = serde_json::from_value(name.into()).unwrap();
            assert_eq!(parsed, protocol);
        }
        // The `upload.protocol` names of Arduino's `boards.txt`.
        let arduino: Protocol = serde_json::from_value("arduino".into()).unwrap();
        assert_eq!(arduino, Protocol::Stk500v1);
        let wiring: Protocol = serde_json::from_value("wiring".into()).unwrap();
        assert_eq!(wiring, Protocol::Stk500v2);
        let manifest = Manifest::parse(
            "[[board]]\nidentifier = \"uno\"\nprotocol = \"arduino\"\nchip = \"atmega328p\"\n",
        )
        .unwrap();
        assert_eq!(manifest.boards[0].protocol, Protocol::Stk500v1);
    }
}
