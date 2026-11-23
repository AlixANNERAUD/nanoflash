//! The layout model: the flash of a board and its named partitions.

mod error;
mod report;
mod resolve;

pub use error::{LayoutError, LayoutErrors};
pub use report::{ImageSize, PartitionReport, Report};
pub use resolve::{ResolvedLayout, ResolvedPartition};

use crate::quantity;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};

/// A whole layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// The flash device.
    pub flash: Flash,
    /// The partitions, in file order.
    #[serde(default, rename = "partition")]
    pub partitions: Vec<Partition>,
}

/// The flash device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawFlash", into = "RawFlash")]
pub struct Flash {
    /// Total size in bytes. With `detect`, the minimum: the size offline tools resolve against.
    pub size: u64,
    /// Whether the size is read from the chip when flashing (`size = "detect"`).
    pub detect: bool,
    /// Default alignment of every partition, in bytes.
    pub alignment: u64,
    /// Address at which the flash is mapped.
    pub base_address: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawFlash {
    #[serde(deserialize_with = "deserialize_flash_size")]
    size: RawFlashSize,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_quantity"
    )]
    minimum: Option<u64>,
    #[serde(
        default = "default_alignment",
        deserialize_with = "deserialize_quantity"
    )]
    alignment: u64,
    #[serde(default, deserialize_with = "deserialize_quantity")]
    base_address: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawFlashSize {
    Bytes(u64),
    Detect,
}

impl Serialize for RawFlashSize {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            RawFlashSize::Bytes(bytes) => serializer.serialize_u64(*bytes),
            RawFlashSize::Detect => serializer.serialize_str("detect"),
        }
    }
}

impl TryFrom<RawFlash> for Flash {
    type Error = String;

    fn try_from(raw: RawFlash) -> Result<Self, String> {
        let (size, detect) = match (raw.size, raw.minimum) {
            (RawFlashSize::Detect, Some(minimum)) => (minimum, true),
            (RawFlashSize::Detect, None) => {
                return Err(String::from(
                    "the flash size \"detect\" needs a \"minimum\", such as minimum = \"4 MiB\"",
                ));
            }
            (RawFlashSize::Bytes(bytes), None) => (bytes, false),
            (RawFlashSize::Bytes(_), Some(_)) => {
                return Err(String::from(
                    "the flash has a \"minimum\" only when its size is \"detect\"",
                ));
            }
        };
        Ok(Flash {
            size,
            detect,
            alignment: raw.alignment,
            base_address: raw.base_address,
        })
    }
}

impl From<Flash> for RawFlash {
    fn from(flash: Flash) -> Self {
        let (size, minimum) = if flash.detect {
            (RawFlashSize::Detect, Some(flash.size))
        } else {
            (RawFlashSize::Bytes(flash.size), None)
        };
        RawFlash {
            size,
            minimum,
            alignment: flash.alignment,
            base_address: flash.base_address,
        }
    }
}

fn deserialize_flash_size<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<RawFlashSize, D::Error> {
    struct FlashSizeVisitor;

    impl Visitor<'_> for FlashSizeVisitor {
        type Value = RawFlashSize;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a size, or \"detect\"")
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<RawFlashSize, E> {
            Ok(RawFlashSize::Bytes(value))
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<RawFlashSize, E> {
            QuantityVisitor.visit_i64(value).map(RawFlashSize::Bytes)
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<RawFlashSize, E> {
            if value.trim_matches(' ') == "detect" {
                Ok(RawFlashSize::Detect)
            } else {
                quantity::parse(value)
                    .map(RawFlashSize::Bytes)
                    .map_err(E::custom)
            }
        }
    }

    deserializer.deserialize_any(FlashSizeVisitor)
}

fn default_alignment() -> u64 {
    0x1000
}

/// What a partition is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Second-stage bootloader.
    Bootloader,
    /// The partition table itself.
    PartitionTable,
    /// The running application.
    Firmware,
    /// A slot receiving updates.
    Update {
        /// The slot number.
        slot: u8,
    },
    /// A file system or data store.
    Storage,
    /// Where crash records are kept.
    CrashRecord,
    /// Anything else, described by family attributes.
    Custom,
}

/// The size of a partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Size {
    /// A fixed number of bytes.
    Bytes(u64),
    /// Everything that is left.
    Remaining,
    /// At least `minimum` bytes, plus a share of the free space by weight.
    Grow {
        /// The least size in bytes.
        minimum: u64,
        /// The weight of the share of the free space.
        grow: core::num::NonZeroU32,
        /// The most size in bytes, if any.
        maximum: Option<u64>,
    },
}

impl Size {
    /// The least size in bytes: the size of `Bytes`, the minimum of `Grow`, `0` for `Remaining`.
    pub fn minimum(&self) -> u64 {
        match self {
            Size::Bytes(bytes) => *bytes,
            Size::Remaining => 0,
            Size::Grow { minimum, .. } => *minimum,
        }
    }

    /// Whether the partition takes a share of the free space.
    pub fn grows(&self) -> bool {
        matches!(self, Size::Remaining | Size::Grow { .. })
    }
}

/// A partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPartition", into = "RawPartition")]
pub struct Partition {
    /// Unique name.
    pub name: String,
    /// What the partition is for.
    pub role: Role,
    /// Fixed offset from the start of the flash, if any.
    pub offset: Option<u64>,
    /// Size of the partition.
    pub size: Size,
    /// Alignment overriding the flash default.
    pub alignment: Option<u64>,
    /// Name of the file system, informative.
    pub filesystem: Option<String>,
    /// Espressif attributes.
    pub espressif: Option<EspressifAttributes>,
    /// MBR attributes.
    pub mbr: Option<MbrAttributes>,
    /// Linker attributes.
    pub linker: Option<LinkerAttributes>,
}

/// Espressif partition table attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EspressifAttributes {
    /// Partition type.
    #[serde(default)]
    pub r#type: Option<EspressifValue>,
    /// Partition subtype.
    #[serde(default)]
    pub subtype: Option<EspressifValue>,
    /// Encrypted flag.
    #[serde(default)]
    pub encrypted: bool,
    /// Read-only flag.
    #[serde(default)]
    pub readonly: bool,
}

/// A type or subtype, by name or by number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EspressifValue {
    /// A well-known name such as `data` or `nvs`.
    Name(String),
    /// A raw number.
    Number(u8),
}

/// MBR partition table attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MbrAttributes {
    /// Partition type byte.
    #[serde(default)]
    pub r#type: Option<u8>,
    /// Bootable flag.
    #[serde(default)]
    pub bootable: bool,
}

/// Linker script attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkerAttributes {
    /// Name of the memory region.
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawRole {
    Bootloader,
    PartitionTable,
    Firmware,
    Update,
    Storage,
    CrashRecord,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPartition {
    name: String,
    role: RawRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    slot: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_quantity"
    )]
    offset: Option<u64>,
    #[serde(deserialize_with = "deserialize_size")]
    size: RawSize,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_quantity"
    )]
    alignment: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filesystem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    espressif: Option<EspressifAttributes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mbr: Option<MbrAttributes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linker: Option<LinkerAttributes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawSize(Size);

impl Serialize for RawSize {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self.0 {
            Size::Bytes(bytes) => serializer.serialize_u64(bytes),
            Size::Remaining => serializer.serialize_str("remaining"),
            Size::Grow {
                minimum,
                grow,
                maximum,
            } => {
                let length = if maximum.is_some() { 3 } else { 2 };
                let mut map = serializer.serialize_map(Some(length))?;
                map.serialize_entry("minimum", &minimum)?;
                map.serialize_entry("grow", &grow.get())?;
                if let Some(maximum) = maximum {
                    map.serialize_entry("maximum", &maximum)?;
                }
                map.end()
            }
        }
    }
}

impl TryFrom<RawPartition> for Partition {
    type Error = String;

    fn try_from(raw: RawPartition) -> Result<Self, String> {
        let role = match (raw.role, raw.slot) {
            (RawRole::Update, Some(slot)) => Role::Update { slot },
            (RawRole::Update, None) => {
                return Err(alloc::format!(
                    "partition \"{}\" has role \"update\" and needs a \"slot\"",
                    raw.name
                ));
            }
            (_, Some(_)) => {
                return Err(alloc::format!(
                    "partition \"{}\" has a \"slot\" but its role is not \"update\"",
                    raw.name
                ));
            }
            (RawRole::Bootloader, None) => Role::Bootloader,
            (RawRole::PartitionTable, None) => Role::PartitionTable,
            (RawRole::Firmware, None) => Role::Firmware,
            (RawRole::Storage, None) => Role::Storage,
            (RawRole::CrashRecord, None) => Role::CrashRecord,
            (RawRole::Custom, None) => Role::Custom,
        };
        Ok(Partition {
            name: raw.name,
            role,
            offset: raw.offset,
            size: raw.size.0,
            alignment: raw.alignment,
            filesystem: raw.filesystem,
            espressif: raw.espressif,
            mbr: raw.mbr,
            linker: raw.linker,
        })
    }
}

impl From<Partition> for RawPartition {
    fn from(partition: Partition) -> Self {
        let (role, slot) = match partition.role {
            Role::Bootloader => (RawRole::Bootloader, None),
            Role::PartitionTable => (RawRole::PartitionTable, None),
            Role::Firmware => (RawRole::Firmware, None),
            Role::Update { slot } => (RawRole::Update, Some(slot)),
            Role::Storage => (RawRole::Storage, None),
            Role::CrashRecord => (RawRole::CrashRecord, None),
            Role::Custom => (RawRole::Custom, None),
        };
        RawPartition {
            name: partition.name,
            role,
            slot,
            offset: partition.offset,
            size: RawSize(partition.size),
            alignment: partition.alignment,
            filesystem: partition.filesystem,
            espressif: partition.espressif,
            mbr: partition.mbr,
            linker: partition.linker,
        }
    }
}

struct QuantityVisitor;

impl Visitor<'_> for QuantityVisitor {
    type Value = u64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a size or offset: an integer, hexadecimal or a number with a unit")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
        Ok(value)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<u64, E> {
        u64::try_from(value)
            .map_err(|_| E::custom(alloc::format!("negative size or offset {value}")))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<u64, E> {
        quantity::parse(value).map_err(E::custom)
    }
}

fn deserialize_quantity<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    deserializer.deserialize_any(QuantityVisitor)
}

fn deserialize_optional_quantity<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    deserialize_quantity(deserializer).map(Some)
}

fn deserialize_size<'de, D: Deserializer<'de>>(deserializer: D) -> Result<RawSize, D::Error> {
    struct SizeVisitor;

    /// A quantity read through `QuantityVisitor`.
    struct Quantity(u64);

    impl<'de> Deserialize<'de> for Quantity {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Quantity, D::Error> {
            deserialize_quantity(deserializer).map(Quantity)
        }
    }

    impl<'de> Visitor<'de> for SizeVisitor {
        type Value = RawSize;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(
                "a size, \"remaining\", or a table with \"minimum\", \"grow\" and \"maximum\"",
            )
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<RawSize, A::Error> {
            let mut minimum: Option<u64> = None;
            let mut grow: Option<i128> = None;
            let mut maximum: Option<u64> = None;
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "minimum" => minimum = Some(map.next_value::<Quantity>()?.0),
                    "grow" => grow = Some(map.next_value::<i128>()?),
                    "maximum" => maximum = Some(map.next_value::<Quantity>()?.0),
                    other => {
                        return Err(de::Error::unknown_field(
                            other,
                            &["minimum", "grow", "maximum"],
                        ));
                    }
                }
            }
            let minimum = minimum.ok_or_else(|| {
                de::Error::custom(
                    "a growing size needs a \"minimum\", such as { minimum = \"1 MiB\", grow = 1 }",
                )
            })?;
            let grow = match grow {
                None => core::num::NonZeroU32::MIN,
                Some(weight) => u32::try_from(weight)
                    .ok()
                    .and_then(core::num::NonZeroU32::new)
                    .ok_or_else(|| {
                        de::Error::custom(
                            "\"grow\" is a weight: a whole number from 1 to 4294967295",
                        )
                    })?,
            };
            Ok(RawSize(Size::Grow {
                minimum,
                grow,
                maximum,
            }))
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<RawSize, E> {
            Ok(RawSize(Size::Bytes(value)))
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<RawSize, E> {
            QuantityVisitor
                .visit_i64(value)
                .map(|bytes| RawSize(Size::Bytes(bytes)))
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<RawSize, E> {
            if value.trim_matches(' ') == "remaining" {
                Ok(RawSize(Size::Remaining))
            } else {
                quantity::parse(value)
                    .map(|bytes| RawSize(Size::Bytes(bytes)))
                    .map_err(E::custom)
            }
        }
    }

    deserializer.deserialize_any(SizeVisitor)
}

/// A layout file that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseError {
    /// The message, with the line and column when the parser gives them.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl core::error::Error for ParseError {}

#[cfg(feature = "toml")]
impl Layout {
    /// Read a layout from the text of a TOML layout file.
    pub fn from_toml(text: &str) -> Result<Layout, ParseError> {
        use alloc::string::ToString;
        toml::from_str(text).map_err(|error| ParseError {
            message: error.to_string(),
        })
    }
}

#[cfg(all(test, feature = "toml"))]
mod tests {
    use super::*;

    fn partition_named<'a>(layout: &'a Layout, name: &str) -> &'a Partition {
        layout.partitions.iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn parses_every_fixture() {
        let esp = Layout::from_toml(include_str!("../../tests/layouts/esp32-s3-8mb.toml")).unwrap();
        assert_eq!(esp.flash.size, 8 << 20);
        assert_eq!(esp.flash.alignment, 0x1000);
        assert_eq!(esp.partitions.len(), 8);
        let names: Vec<&str> = esp.partitions.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "bootloader",
                "partition-table",
                "nvs",
                "otadata",
                "application",
                "ota-0",
                "ota-1",
                "storage"
            ]
        );
        assert_eq!(
            partition_named(&esp, "ota-1").role,
            Role::Update { slot: 1 }
        );
        assert_eq!(partition_named(&esp, "storage").size, Size::Remaining);
        assert_eq!(
            partition_named(&esp, "partition-table").offset,
            Some(0x8000)
        );
        assert_eq!(
            partition_named(&esp, "application").alignment,
            Some(0x10000)
        );
        assert_eq!(
            partition_named(&esp, "otadata").espressif,
            Some(EspressifAttributes {
                r#type: Some(EspressifValue::Name("data".into())),
                subtype: Some(EspressifValue::Name("ota".into())),
                encrypted: false,
                readonly: false,
            })
        );

        let nrf =
            Layout::from_toml(include_str!("../../tests/layouts/nrf52840-arduino.toml")).unwrap();
        assert_eq!(nrf.flash.size, 1 << 20);
        assert_eq!(
            partition_named(&nrf, "bootloader").size,
            Size::Bytes(0x10000)
        );
        assert_eq!(partition_named(&nrf, "application").size, Size::Remaining);
        assert_eq!(partition_named(&nrf, "storage").offset, Some(0xF0000));

        let sd = Layout::from_toml(include_str!("../../tests/layouts/sd-card.toml")).unwrap();
        assert_eq!(sd.flash.alignment, 1 << 20);
        assert_eq!(partition_named(&sd, "partition-table").alignment, Some(512));
        assert_eq!(
            partition_named(&sd, "boot").filesystem.as_deref(),
            Some("fat")
        );
        assert_eq!(
            partition_named(&sd, "data").mbr,
            Some(MbrAttributes {
                r#type: Some(0x83),
                bootable: false
            })
        );
    }

    #[test]
    fn update_requires_slot_and_slot_requires_update() {
        let header = "[flash]\nsize = \"1 MiB\"\n[[partition]]\nname = \"a\"\nsize = 4096\n";
        let error = Layout::from_toml(&alloc::format!("{header}role = \"update\"\n")).unwrap_err();
        assert!(error.message.contains("slot"), "{error}");
        let error = Layout::from_toml(&alloc::format!("{header}role = \"firmware\"\nslot = 0\n"))
            .unwrap_err();
        assert!(error.message.contains("slot"), "{error}");
        assert!(
            Layout::from_toml(&alloc::format!("{header}role = \"update\"\nslot = 2\n")).is_ok()
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        let text = "[flash]\nsize = \"1 MiB\"\n[[partition]]\nname = \"a\"\nrole = \"firmware\"\nsize = 4096\ncolour = 1\n";
        let error = Layout::from_toml(text).unwrap_err();
        assert!(error.message.contains("colour"), "{error}");
        assert!(Layout::from_toml("[flash]\nsize = 1\ncolour = 1\n").is_err());
    }

    #[test]
    fn reports_bad_quantities() {
        let error = Layout::from_toml("[flash]\nsize = \"4KB\"\n").unwrap_err();
        assert!(error.message.contains("4 KiB"), "{error}");
        assert!(Layout::from_toml("[flash]\nsize = -1\n").is_err());
    }

    #[test]
    fn parses_a_detected_flash() {
        let layout =
            Layout::from_toml(include_str!("../../tests/layouts/esp32-s3-detect.toml")).unwrap();
        assert!(layout.flash.detect);
        assert_eq!(layout.flash.size, 4 << 20);
        let error = Layout::from_toml("[flash]\nsize = \"detect\"\n").unwrap_err();
        assert!(error.message.contains("minimum"), "{error}");
        let error =
            Layout::from_toml("[flash]\nsize = \"8 MiB\"\nminimum = \"4 MiB\"\n").unwrap_err();
        assert!(error.message.contains("detect"), "{error}");
        let growing = Layout::from_toml(include_str!("../../tests/layouts/growing.toml")).unwrap();
        assert!(growing.flash.detect);
        assert_eq!(growing.flash.size, 512 << 10);
        assert!(partition_named(&growing, "storage").size.grows());
        let fixed = Layout::from_toml("[flash]\nsize = \"8 MiB\"\n").unwrap();
        assert!(!fixed.flash.detect);
    }

    fn size_of_text(size: &str) -> Result<Size, ParseError> {
        let text = alloc::format!(
            "[flash]\nsize = \"1 MiB\"\n[[partition]]\nname = \"a\"\nrole = \"storage\"\nsize = {size}\n"
        );
        Layout::from_toml(&text).map(|layout| layout.partitions[0].size)
    }

    #[test]
    fn parses_growing_sizes() {
        assert_eq!(
            size_of_text("{ minimum = \"1 MiB\", grow = 3, maximum = \"2 MiB\" }").unwrap(),
            Size::Grow {
                minimum: 0x100000,
                grow: core::num::NonZeroU32::new(3).unwrap(),
                maximum: Some(0x200000),
            }
        );
        assert_eq!(
            size_of_text("{ minimum = \"64 KiB\" }").unwrap(),
            Size::Grow {
                minimum: 0x10000,
                grow: core::num::NonZeroU32::MIN,
                maximum: None,
            }
        );
        let error = size_of_text("{ grow = 2 }").unwrap_err();
        assert!(error.message.contains("minimum"), "{error}");
        let error = size_of_text("{ minimum = 4096, grow = 0 }").unwrap_err();
        assert!(error.message.contains("grow"), "{error}");
        let error = size_of_text("{ minimum = 4096, grow = -1 }").unwrap_err();
        assert!(error.message.contains("grow"), "{error}");
        let error = size_of_text("{ minimum = 4096, colour = 1 }").unwrap_err();
        assert!(error.message.contains("colour"), "{error}");
    }

    #[test]
    fn size_helpers_cover_the_variants() {
        let grow = Size::Grow {
            minimum: 4096,
            grow: core::num::NonZeroU32::new(3).unwrap(),
            maximum: None,
        };
        assert_eq!(Size::Bytes(8).minimum(), 8);
        assert_eq!(Size::Remaining.minimum(), 0);
        assert_eq!(grow.minimum(), 4096);
        assert!(!Size::Bytes(8).grows());
        assert!(Size::Remaining.grows());
        assert!(grow.grows());
    }
}
