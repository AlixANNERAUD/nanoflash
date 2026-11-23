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
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Flash {
    /// Total size in bytes.
    #[serde(deserialize_with = "deserialize_quantity")]
    pub size: u64,
    /// Default alignment of every partition, in bytes.
    #[serde(
        default = "default_alignment",
        deserialize_with = "deserialize_quantity"
    )]
    pub alignment: u64,
    /// Address at which the flash is mapped.
    #[serde(default, deserialize_with = "deserialize_quantity")]
    pub base_address: u64,
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
pub enum Size {
    /// A fixed number of bytes.
    Bytes(u64),
    /// Everything that is left.
    Remaining,
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
        match self.0 {
            Size::Bytes(bytes) => serializer.serialize_u64(bytes),
            Size::Remaining => serializer.serialize_str("remaining"),
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

    impl Visitor<'_> for SizeVisitor {
        type Value = RawSize;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a size, or \"remaining\"")
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}
