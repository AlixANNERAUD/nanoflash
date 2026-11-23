//! The Espressif partition table, as ESP-IDF reads it from flash.

use super::TableError;
use crate::layout::{EspressifValue, Partition, ResolvedLayout, ResolvedPartition, Role};
use alloc::format;
use alloc::vec::Vec;
use md5::{Digest, Md5};

/// The size of the table in flash, entries, checksum row and padding included.
pub const TABLE_SIZE: usize = 0xC00;

const WRITER: &str = "espressif";
const MAXIMUM_ENTRIES: usize = 95;
const LABEL_SIZE: usize = 16;
const APP_ALIGNMENT: u64 = 0x10000;
const SECTOR_ALIGNMENT: u64 = 0x1000;
const OTA_DATA_SIZE: u64 = 0x2000;
const APP: u8 = 0x00;
const DATA: u8 = 0x01;
const FACTORY: u8 = 0x00;
const OTA_SLOT_BASE: u8 = 0x10;
const OTA_DATA: u8 = 0x00;

/// Write the binary partition table of a layout.
///
/// The result is exactly [`TABLE_SIZE`] bytes: the entries sorted by offset, the checksum row and
/// the padding. The bootloader and the partition table are not entries.
pub fn write(layout: &ResolvedLayout) -> Result<Vec<u8>, TableError> {
    let table = layout
        .partitions
        .iter()
        .find(|partition| partition.definition.role == Role::PartitionTable)
        .ok_or(TableError::MissingPartition {
            writer: WRITER,
            role: "partition-table",
            required_by: None,
        })?;
    if table.size < TABLE_SIZE as u64 {
        return Err(TableError::TableDoesNotFit {
            writer: WRITER,
            partition: table.definition.name.clone(),
            size: table.size,
            required: TABLE_SIZE as u64,
        });
    }

    let mut partitions: Vec<&ResolvedPartition> = layout
        .partitions
        .iter()
        .filter(|partition| {
            !matches!(
                partition.definition.role,
                Role::Bootloader | Role::PartitionTable
            )
        })
        .collect();
    partitions.sort_by_key(|partition| partition.offset);
    if partitions.len() > MAXIMUM_ENTRIES {
        return Err(TableError::TooManyPartitions {
            writer: WRITER,
            count: partitions.len(),
            maximum: MAXIMUM_ENTRIES,
        });
    }

    let mut bytes = Vec::with_capacity(TABLE_SIZE);
    let mut factory: Option<&str> = None;
    let mut has_otadata = false;
    let mut first_update: Option<&str> = None;
    for partition in partitions {
        let name = partition.definition.name.as_str();
        let (kind, subtype) = types(&partition.definition)?;
        let misaligned = |what, value, required| TableError::Misaligned {
            writer: WRITER,
            partition: name.into(),
            what,
            value,
            required,
        };
        if kind == APP && partition.offset % APP_ALIGNMENT != 0 {
            return Err(misaligned("offset", partition.offset, APP_ALIGNMENT));
        }
        if partition.offset % SECTOR_ALIGNMENT != 0 {
            return Err(misaligned("offset", partition.offset, SECTOR_ALIGNMENT));
        }
        if kind == APP && partition.size % SECTOR_ALIGNMENT != 0 {
            return Err(misaligned("size", partition.size, SECTOR_ALIGNMENT));
        }
        if kind == DATA && subtype == OTA_DATA {
            if partition.size != OTA_DATA_SIZE {
                return Err(TableError::WrongSize {
                    writer: WRITER,
                    partition: name.into(),
                    size: partition.size,
                    required: OTA_DATA_SIZE,
                    what: "the otadata partition",
                });
            }
            has_otadata = true;
        }
        if matches!(partition.definition.role, Role::Update { .. }) && first_update.is_none() {
            first_update = Some(name);
        }
        if (kind, subtype) == (APP, FACTORY) {
            if let Some(first) = factory {
                return Err(TableError::SeveralFirmware {
                    writer: WRITER,
                    first: first.into(),
                    second: name.into(),
                });
            }
            factory = Some(name);
        }
        let out_of_range = || TableError::OutOfRange {
            writer: WRITER,
            partition: name.into(),
        };
        let offset = u32::try_from(partition.offset).map_err(|_| out_of_range())?;
        let size = u32::try_from(partition.size).map_err(|_| out_of_range())?;
        let label = label(name)?;
        let attributes = partition.definition.espressif.as_ref();
        let flags = u32::from(attributes.is_some_and(|attributes| attributes.encrypted))
            | u32::from(attributes.is_some_and(|attributes| attributes.readonly)) << 1;

        bytes.extend_from_slice(&[0xAA, 0x50, kind, subtype]);
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&label);
        bytes.extend_from_slice(&flags.to_le_bytes());
    }
    match first_update {
        Some(update) if !has_otadata => {
            return Err(TableError::MissingPartition {
                writer: WRITER,
                role: "otadata (data/ota)",
                required_by: Some(update.into()),
            });
        }
        _ => {}
    }

    let digest = Md5::digest(&bytes);
    bytes.extend_from_slice(&[0xEB, 0xEB]);
    bytes.extend_from_slice(&[0xFF; 14]);
    bytes.extend_from_slice(&digest);
    bytes.resize(TABLE_SIZE, 0xFF);
    Ok(bytes)
}

fn label(name: &str) -> Result<[u8; LABEL_SIZE], TableError> {
    if !name.is_ascii() {
        return Err(TableError::NotAscii {
            writer: WRITER,
            partition: name.into(),
        });
    }
    if name.len() > LABEL_SIZE {
        return Err(TableError::LabelTooLong {
            writer: WRITER,
            partition: name.into(),
        });
    }
    let mut label = [0u8; LABEL_SIZE];
    for (slot, byte) in label.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    Ok(label)
}

fn missing(partition: &Partition, attribute: &'static str) -> TableError {
    TableError::MissingAttribute {
        partition: partition.name.clone(),
        writer: WRITER,
        attribute,
    }
}

/// The type and subtype bytes of a partition.
fn types(partition: &Partition) -> Result<(u8, u8), TableError> {
    let attributes = partition.espressif.as_ref();
    let type_override = attributes.and_then(|attributes| attributes.r#type.as_ref());
    let subtype_override = attributes.and_then(|attributes| attributes.subtype.as_ref());

    let (default_type, default_subtype) = match partition.role {
        Role::Firmware => (Some(APP), Some(FACTORY)),
        Role::Update { slot } => {
            let subtype = (slot < 16).then_some(OTA_SLOT_BASE + slot);
            if subtype.is_none() && subtype_override.is_none() {
                return Err(TableError::OutOfRange {
                    writer: WRITER,
                    partition: partition.name.clone(),
                });
            }
            (Some(APP), subtype)
        }
        Role::Storage => {
            let subtype = match partition.filesystem.as_deref() {
                Some("fat") => Some(0x81),
                Some("spiffs") => Some(0x82),
                Some("littlefs") => Some(0x83),
                _ => None,
            };
            (Some(DATA), subtype)
        }
        Role::CrashRecord => (Some(DATA), Some(0x03)),
        Role::Custom | Role::Bootloader | Role::PartitionTable => (None, None),
    };

    let kind = match type_override {
        Some(value) => resolve_type(partition, value)?,
        None => default_type.ok_or_else(|| missing(partition, "type"))?,
    };
    let subtype = match subtype_override {
        Some(value) => resolve_subtype(partition, kind, value)?,
        None => default_subtype.ok_or_else(|| missing(partition, "subtype"))?,
    };
    Ok((kind, subtype))
}

fn unknown(partition: &Partition, attribute: &'static str, value: &EspressifValue) -> TableError {
    TableError::UnknownValue {
        writer: WRITER,
        partition: partition.name.clone(),
        attribute,
        value: match value {
            EspressifValue::Name(name) => name.clone(),
            EspressifValue::Number(number) => format!("{number:#x}"),
        },
    }
}

fn resolve_type(partition: &Partition, value: &EspressifValue) -> Result<u8, TableError> {
    match value {
        EspressifValue::Number(number) => Ok(*number),
        EspressifValue::Name(name) => match name.as_str() {
            "app" => Ok(APP),
            "data" => Ok(DATA),
            _ => Err(unknown(partition, "type", value)),
        },
    }
}

fn resolve_subtype(
    partition: &Partition,
    kind: u8,
    value: &EspressifValue,
) -> Result<u8, TableError> {
    let name = match value {
        EspressifValue::Number(number) => return Ok(*number),
        EspressifValue::Name(name) => name.as_str(),
    };
    let found = match kind {
        APP => match name {
            "factory" => Some(FACTORY),
            "test" => Some(0x20),
            _ => name
                .strip_prefix("ota_")
                .and_then(|slot| slot.parse::<u8>().ok())
                .filter(|slot| *slot < 16 && format!("ota_{slot}") == name)
                .map(|slot| OTA_SLOT_BASE + slot),
        },
        DATA => match name {
            "ota" => Some(OTA_DATA),
            "phy" => Some(0x01),
            "nvs" => Some(0x02),
            "coredump" => Some(0x03),
            "nvs_keys" => Some(0x04),
            "efuse" => Some(0x05),
            "undefined" => Some(0x06),
            "esphttpd" => Some(0x80),
            "fat" => Some(0x81),
            "spiffs" => Some(0x82),
            "littlefs" => Some(0x83),
            _ => None,
        },
        _ => None,
    };
    found.ok_or_else(|| unknown(partition, "subtype", value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{
        EspressifAttributes, EspressifValue, Flash, Layout, Partition, Role, Size,
    };
    use alloc::string::{String, ToString};
    use alloc::vec;
    #[cfg(feature = "toml")]
    use md5::{Digest, Md5};
    use proptest::prelude::*;

    #[cfg(feature = "toml")]
    fn fixture(name: &str) -> ResolvedLayout {
        let text = match name {
            "esp32-s3-8mb" => include_str!("../../tests/layouts/esp32-s3-8mb.toml"),
            other => panic!("no fixture {other}"),
        };
        Layout::from_toml(text).unwrap().resolve().unwrap()
    }

    fn partition(name: &str, role: Role, offset: Option<u64>, size: u64) -> Partition {
        Partition {
            name: name.to_string(),
            role,
            offset,
            size: Size::Bytes(size),
            alignment: None,
            filesystem: None,
            espressif: None,
            mbr: None,
            linker: None,
        }
    }

    fn with_espressif(
        mut partition: Partition,
        ty: Option<&str>,
        subtype: Option<&str>,
    ) -> Partition {
        partition.espressif = Some(EspressifAttributes {
            r#type: ty.map(|value| EspressifValue::Name(value.to_string())),
            subtype: subtype.map(|value| EspressifValue::Name(value.to_string())),
            encrypted: false,
            readonly: false,
        });
        partition
    }

    fn table_partition() -> Partition {
        partition(
            "partition-table",
            Role::PartitionTable,
            Some(0x8000),
            0x1000,
        )
    }

    fn resolve(partitions: Vec<Partition>) -> ResolvedLayout {
        Layout {
            flash: Flash {
                size: 16 << 20,
                alignment: 0x1000,
                detect: false,
                base_address: 0,
            },
            partitions,
        }
        .resolve()
        .unwrap()
    }

    fn parse(bytes: &[u8]) -> esp_idf_part::PartitionTable {
        esp_idf_part::PartitionTable::try_from_bytes(bytes.to_vec()).unwrap()
    }

    #[cfg(feature = "toml")]
    #[test]
    fn esp32_fixture_round_trips_through_esp_idf_part() {
        let bytes = write(&fixture("esp32-s3-8mb")).unwrap();
        assert_eq!(bytes.len(), 0xC00);
        let table = parse(&bytes);
        let actual: Vec<(String, u8, u8, u32, u32)> = table
            .partitions()
            .iter()
            .map(|entry| {
                (
                    entry.name(),
                    entry.ty().into(),
                    entry.subtype().into(),
                    entry.offset(),
                    entry.size(),
                )
            })
            .collect();
        let expected = vec![
            ("nvs".to_string(), 1, 0x02, 0x9000, 0x6000),
            ("otadata".to_string(), 1, 0x00, 0xF000, 0x2000),
            ("application".to_string(), 0, 0x00, 0x20000, 0x100000),
            ("ota-0".to_string(), 0, 0x10, 0x120000, 0x100000),
            ("ota-1".to_string(), 0, 0x11, 0x220000, 0x100000),
            (
                "storage".to_string(),
                1,
                0x83,
                0x320000,
                0x800000 - 0x320000,
            ),
        ];
        assert_eq!(actual, expected);
    }

    #[cfg(feature = "toml")]
    #[test]
    fn md5_row_follows_the_entries() {
        let bytes = write(&fixture("esp32-s3-8mb")).unwrap();
        let entries = 6 * 32;
        assert_eq!(bytes.get(entries..entries + 2), Some(&[0xEB, 0xEB][..]));
        assert!(bytes[entries + 2..entries + 16].iter().all(|&b| b == 0xFF));
        let digest = Md5::digest(&bytes[..entries]);
        assert_eq!(&bytes[entries + 16..entries + 32], digest.as_slice());
        assert!(bytes[entries + 32..].iter().all(|&b| b == 0xFF));
        assert_eq!(&bytes[..2], &[0xAA, 0x50]);
    }

    #[test]
    fn flags_are_written() {
        let mut data = with_espressif(
            partition("keys", Role::Custom, Some(0x9000), 0x1000),
            Some("data"),
            Some("nvs_keys"),
        );
        if let Some(attributes) = data.espressif.as_mut() {
            attributes.encrypted = true;
            attributes.readonly = true;
        }
        let layout = resolve(vec![
            table_partition(),
            data,
            partition("app", Role::Firmware, Some(0x10000), 0x10000),
        ]);
        let bytes = write(&layout).unwrap();
        assert_eq!(&bytes[28..32], &[3, 0, 0, 0]);
        assert_eq!(bytes[3], 0x04);
    }

    #[test]
    fn rejects_long_and_non_ascii_labels() {
        for (name, expected) in [("a-seventeen-bytes", "long"), ("données", "ascii")] {
            let layout = resolve(vec![
                table_partition(),
                partition(name, Role::Firmware, Some(0x10000), 0x10000),
            ]);
            let error = write(&layout).unwrap_err();
            match expected {
                "long" => assert_eq!(
                    error,
                    TableError::LabelTooLong {
                        writer: "espressif",
                        partition: name.into()
                    }
                ),
                _ => assert_eq!(
                    error,
                    TableError::NotAscii {
                        writer: "espressif",
                        partition: name.into()
                    }
                ),
            }
            assert!(error.to_string().contains(name), "{error}");
        }
        let exact = resolve(vec![
            table_partition(),
            partition("0123456789abcdef", Role::Firmware, Some(0x10000), 0x10000),
        ]);
        assert_eq!(parse(&write(&exact).unwrap()).partitions().len(), 1);
    }

    trait WithOta {
        fn with_ota(self) -> Self;
    }

    impl WithOta for Partition {
        fn with_ota(self) -> Self {
            with_espressif(self, Some("data"), Some("ota"))
        }
    }

    #[test]
    fn custom_needs_attributes() {
        let layout = resolve(vec![
            table_partition(),
            partition("blob", Role::Custom, Some(0x10000), 0x1000),
        ]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::MissingAttribute {
                partition: "blob".into(),
                writer: "espressif",
                attribute: "type"
            }
        );
        let layout = resolve(vec![
            table_partition(),
            with_espressif(
                partition("blob", Role::Custom, Some(0x10000), 0x1000),
                Some("data"),
                None,
            ),
        ]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::MissingAttribute {
                partition: "blob".into(),
                writer: "espressif",
                attribute: "subtype"
            }
        );
    }

    #[test]
    fn storage_needs_a_known_file_system() {
        let mut storage = partition("storage", Role::Storage, Some(0x10000), 0x1000);
        storage.filesystem = Some("ext4".into());
        let layout = resolve(vec![table_partition(), storage.clone()]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::MissingAttribute {
                partition: "storage".into(),
                writer: "espressif",
                attribute: "subtype"
            }
        );
        let layout = resolve(vec![
            table_partition(),
            with_espressif(storage, None, Some("0x90")),
        ]);
        assert!(matches!(
            write(&layout).unwrap_err(),
            TableError::UnknownValue { .. }
        ));
    }

    #[test]
    fn update_slots_need_otadata() {
        let layout = resolve(vec![
            table_partition(),
            partition("ota-0", Role::Update { slot: 0 }, Some(0x10000), 0x10000),
        ]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::MissingPartition {
                writer: "espressif",
                role: "otadata (data/ota)",
                required_by: Some("ota-0".into())
            }
        );
        assert!(
            write(&layout)
                .unwrap_err()
                .to_string()
                .contains("required by partition \"ota-0\"")
        );
    }

    #[test]
    fn update_slot_must_be_below_sixteen() {
        let layout = resolve(vec![
            table_partition(),
            partition("ota-16", Role::Update { slot: 16 }, Some(0x10000), 0x10000),
        ]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::OutOfRange {
                writer: "espressif",
                partition: "ota-16".into()
            }
        );
    }

    #[test]
    fn app_must_be_64k_aligned() {
        let layout = resolve(vec![
            table_partition(),
            partition("app", Role::Firmware, Some(0x11000), 0x10000),
        ]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::Misaligned {
                writer: "espressif",
                partition: "app".into(),
                what: "offset",
                value: 0x11000,
                required: 0x10000
            }
        );
    }

    #[test]
    fn app_size_must_be_a_multiple_of_4k() {
        let layout = resolve(vec![
            table_partition(),
            partition("app", Role::Firmware, Some(0x10000), 0x101800),
        ]);
        let error = write(&layout).unwrap_err();
        assert_eq!(
            error,
            TableError::Misaligned {
                writer: "espressif",
                partition: "app".into(),
                what: "size",
                value: 0x101800,
                required: 0x1000
            }
        );
        assert!(
            error
                .to_string()
                .contains("size 0x101800 is not a multiple of 0x1000"),
            "{error}"
        );
    }

    #[test]
    fn every_offset_must_be_a_multiple_of_4k() {
        let mut blob = with_espressif(
            partition("blob", Role::Custom, Some(0x113200), 0x200),
            Some("data"),
            Some("nvs"),
        );
        blob.alignment = Some(512);
        let layout = resolve(vec![table_partition(), blob]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::Misaligned {
                writer: "espressif",
                partition: "blob".into(),
                what: "offset",
                value: 0x113200,
                required: 0x1000
            }
        );
    }

    #[test]
    fn otadata_must_be_8k() {
        let layout = resolve(vec![
            table_partition(),
            partition("otadata", Role::Custom, Some(0x9000), 0x1000).with_ota(),
        ]);
        let error = write(&layout).unwrap_err();
        assert_eq!(
            error,
            TableError::WrongSize {
                writer: "espressif",
                partition: "otadata".into(),
                size: 0x1000,
                required: 0x2000,
                what: "the otadata partition"
            }
        );
        assert!(error.to_string().contains("0x2000"), "{error}");
    }

    #[test]
    fn offsets_must_fit_thirty_two_bits() {
        let layout = Layout {
            flash: Flash {
                size: 8 << 30,
                alignment: 0x1000,
                detect: false,
                base_address: 0,
            },
            partitions: vec![
                table_partition(),
                partition("app", Role::Firmware, Some(5 << 30), 0x10000),
            ],
        }
        .resolve()
        .unwrap();
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::OutOfRange {
                writer: "espressif",
                partition: "app".into()
            }
        );
    }

    #[test]
    fn table_partition_is_required_and_big_enough() {
        let app = partition("app", Role::Firmware, Some(0x10000), 0x10000);
        assert_eq!(
            write(&resolve(vec![app.clone()])).unwrap_err(),
            TableError::MissingPartition {
                writer: "espressif",
                role: "partition-table",
                required_by: None
            }
        );
        let small = partition("partition-table", Role::PartitionTable, Some(0x8000), 0x800);
        assert_eq!(
            write(&resolve(vec![small, app])).unwrap_err(),
            TableError::TableDoesNotFit {
                writer: "espressif",
                partition: "partition-table".into(),
                size: 0x800,
                required: 0xC00
            }
        );
    }

    #[test]
    fn two_factory_applications_are_refused() {
        let layout = resolve(vec![
            table_partition(),
            partition("a", Role::Firmware, Some(0x10000), 0x10000),
            partition("b", Role::Firmware, Some(0x20000), 0x10000),
        ]);
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::SeveralFirmware {
                writer: "espressif",
                first: "a".into(),
                second: "b".into()
            }
        );
    }

    #[test]
    fn too_many_partitions_are_refused() {
        let mut partitions = vec![table_partition()];
        for index in 0..96u64 {
            partitions.push(with_espressif(
                partition(
                    &alloc::format!("d{index}"),
                    Role::Custom,
                    Some(0x10000 + index * 0x1000),
                    0x1000,
                ),
                Some("data"),
                Some("nvs"),
            ));
        }
        assert_eq!(
            write(&resolve(partitions)).unwrap_err(),
            TableError::TooManyPartitions {
                writer: "espressif",
                count: 96,
                maximum: 95
            }
        );
    }

    proptest! {
        #[test]
        fn random_valid_tables_round_trip(
            apps in 1usize..=6,
            sizes in proptest::collection::vec(1u64..=4, 12),
            data_kinds in proptest::collection::vec(0usize..5, 0..=6),
        ) {
            let mut partitions = vec![
                table_partition(),
                partition("otadata", Role::Custom, Some(0x9000), 0x2000).with_ota(),
            ];
            for (index, size) in sizes.iter().enumerate().take(apps) {
                let role = if index == 0 {
                    Role::Firmware
                } else {
                    Role::Update { slot: (index - 1) as u8 }
                };
                let mut app = partition(&alloc::format!("app{index}"), role, None, size * 0x10000);
                app.alignment = Some(0x10000);
                partitions.push(app);
            }
            let subtypes = ["nvs", "phy", "spiffs", "fat", "coredump"];
            for (index, kind) in data_kinds.iter().enumerate() {
                partitions.push(with_espressif(
                    partition(&alloc::format!("data{index}"), Role::Custom, None, sizes[6 + index] * 0x1000),
                    Some("data"),
                    Some(subtypes[*kind]),
                ));
            }
            // Apps are placed first, then data, so offsets are in file order after resolution.
            let layout = resolve(partitions);
            let bytes = write(&layout).unwrap();
            prop_assert_eq!(bytes.len(), 0xC00);
            let table = parse(&bytes);
            let mut expected: Vec<(String, u8, u8, u64, u64)> = layout
                .partitions
                .iter()
                .filter(|p| p.definition.role != Role::PartitionTable)
                .map(|p| {
                    let (kind, subtype) = types(&p.definition).unwrap();
                    (p.definition.name.clone(), kind, subtype, p.offset, p.size)
                })
                .collect();
            expected.sort_by_key(|entry| entry.3);
            let actual: Vec<(String, u8, u8, u64, u64)> = table
                .partitions()
                .iter()
                .map(|e| {
                    (
                        e.name(),
                        e.ty().into(),
                        e.subtype().into(),
                        u64::from(e.offset()),
                        u64::from(e.size()),
                    )
                })
                .collect();
            prop_assert_eq!(actual, expected);
        }
    }
}
