//! The master boot record, as firmware and operating systems read it from the first sector.

use super::TableError;
use crate::layout::{ResolvedLayout, ResolvedPartition, Role};
use alloc::vec::Vec;
use mbrman::{CHS, MBRHeader};

/// The size of the master boot record in flash.
pub const TABLE_SIZE: usize = 512;

const WRITER: &str = "mbr";
const SECTOR: u64 = 512;
const MAXIMUM_ENTRIES: usize = 4;
const BOOTABLE: u8 = 0x80;
const FAT32_LBA: u8 = 0x0C;
/// The CHS address of "beyond the CHS range" (cylinder 1023, head 254, sector 63), which is what
/// `sfdisk` writes when the LBA fields alone are meaningful.
const CHS_UNUSED: CHS = CHS {
    cylinder: 1023,
    head: 254,
    sector: 63,
};

/// Write the master boot record of a layout.
///
/// The boot code and the disk signature are left zero. The entries are sorted by offset. The
/// bootloader and the partition table are not entries.
pub fn write(layout: &ResolvedLayout) -> Result<[u8; TABLE_SIZE], TableError> {
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

    let first = partitions
        .first()
        .copied()
        .map(|partition| partition.definition.name.as_str());
    let mut header = MBRHeader::new([0; 4]);
    for (index, partition) in partitions.into_iter().enumerate() {
        let name = partition.definition.name.as_str();
        let misaligned = |what, value| TableError::Misaligned {
            partition: name.into(),
            writer: WRITER,
            what,
            value,
            required: SECTOR,
        };
        if partition.offset % SECTOR != 0 {
            return Err(misaligned("offset", partition.offset));
        }
        if partition.offset < SECTOR {
            return Err(TableError::CoversFirstSector {
                partition: name.into(),
                writer: WRITER,
                offset: partition.offset,
                minimum: SECTOR,
            });
        }
        if partition.size % SECTOR != 0 {
            return Err(misaligned("size", partition.size));
        }
        let out_of_range = || TableError::OutOfRange {
            partition: name.into(),
            writer: WRITER,
        };
        let first = u32::try_from(partition.offset / SECTOR).map_err(|_| out_of_range())?;
        let count = u32::try_from(partition.size / SECTOR).map_err(|_| out_of_range())?;
        first.checked_add(count).ok_or_else(out_of_range)?;

        let attributes = partition.definition.mbr.as_ref();
        let kind = match attributes.and_then(|attributes| attributes.r#type) {
            Some(kind) => kind,
            None if partition.definition.role == Role::Storage
                && partition.definition.filesystem.as_deref() == Some("fat") =>
            {
                FAT32_LBA
            }
            None => {
                return Err(TableError::MissingAttribute {
                    partition: name.into(),
                    writer: WRITER,
                    attribute: "type",
                });
            }
        };
        let status = if attributes.is_some_and(|attributes| attributes.bootable) {
            BOOTABLE
        } else {
            0x00
        };

        // The entry count is checked above, so the index is one of the four slots.
        if let Some(entry) = header.get_mut(index + 1) {
            entry.boot = status;
            entry.first_chs = CHS_UNUSED;
            entry.sys = kind;
            entry.last_chs = CHS_UNUSED;
            entry.starting_lba = first;
            entry.sectors = count;
        }
    }
    // The boot flags written above are the only thing the encoder validates, and they are valid.
    header.to_bytes().map_err(|_| encoding_error(first))
}

/// The error of an encoding that cannot fail: it names the first entry of the table, else the
/// writer, so that the message names something.
fn encoding_error(first: Option<&str>) -> TableError {
    TableError::OutOfRange {
        partition: first.unwrap_or(WRITER).into(),
        writer: WRITER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encoding_error_names_a_partition() {
        assert_eq!(
            encoding_error(Some("storage")),
            TableError::OutOfRange {
                partition: "storage".into(),
                writer: "mbr"
            }
        );
        assert_eq!(
            encoding_error(None),
            TableError::OutOfRange {
                partition: "mbr".into(),
                writer: "mbr"
            }
        );
    }
    use crate::layout::{Flash, Layout, MbrAttributes, Partition, Size};
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;
    #[cfg(feature = "toml")]
    use std::io::Cursor;

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

    fn typed(mut partition: Partition, kind: Option<u8>, bootable: bool) -> Partition {
        partition.mbr = Some(MbrAttributes {
            r#type: kind,
            bootable,
        });
        partition
    }

    fn resolve(alignment: u64, partitions: Vec<Partition>) -> ResolvedLayout {
        Layout {
            flash: Flash {
                size: 64 << 20,
                alignment,
                base_address: 0,
            },
            partitions,
        }
        .resolve()
        .unwrap()
    }

    fn table_partition() -> Partition {
        partition("partition-table", Role::PartitionTable, Some(0), 512)
    }

    #[cfg(feature = "toml")]
    fn fixture() -> ResolvedLayout {
        Layout::from_toml(include_str!("../../tests/layouts/sd-card.toml"))
            .unwrap()
            .resolve()
            .unwrap()
    }

    #[cfg(feature = "toml")]
    #[test]
    fn sd_card_fixture_round_trips_through_mbrman() {
        let layout = fixture();
        let sector = write(&layout).unwrap();
        let mut image = vec![0u8; 64 << 20];
        image[..512].copy_from_slice(&sector);
        let mbr = mbrman::MBR::read_from(&mut Cursor::new(image), 512).unwrap();
        let used: Vec<(u8, u8, u32, u32)> = mbr
            .iter()
            .filter(|(_, entry)| entry.is_used())
            .map(|(_, entry)| (entry.boot, entry.sys, entry.starting_lba, entry.sectors))
            .collect();
        assert_eq!(
            used,
            vec![
                (0x00, 0x0C, 0x100000 / 512, (16 << 20) / 512),
                (0x00, 0x83, 0x1100000 / 512, ((64 << 20) - 0x1100000) / 512),
            ]
        );
    }

    #[cfg(feature = "toml")]
    #[test]
    fn signature_and_chs() {
        let sector = write(&fixture()).unwrap();
        assert_eq!(&sector[510..], &[0x55, 0xAA]);
        assert!(sector[..0x1BE].iter().all(|&byte| byte == 0));
        for entry in sector[0x1BE..510].chunks(16).take(2) {
            assert_eq!(&entry[1..4], &[0xFE, 0xFF, 0xFF]);
            assert_eq!(&entry[5..8], &[0xFE, 0xFF, 0xFF]);
        }
        assert!(sector[0x1BE + 32..510].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn bootable_flag_is_written() {
        let layout = resolve(
            512,
            vec![
                table_partition(),
                typed(
                    partition("boot", Role::Custom, Some(0x100000), 0x100000),
                    Some(0x0C),
                    true,
                ),
            ],
        );
        assert_eq!(write(&layout).unwrap()[0x1BE], 0x80);
    }

    #[test]
    fn five_partitions_are_too_many() {
        let mut partitions = vec![table_partition()];
        for index in 0..5u64 {
            partitions.push(typed(
                partition(
                    &alloc::format!("p{index}"),
                    Role::Custom,
                    Some(0x100000 + index * 0x100000),
                    0x100000,
                ),
                Some(0x83),
                false,
            ));
        }
        assert_eq!(
            write(&resolve(512, partitions)).unwrap_err(),
            TableError::TooManyPartitions {
                writer: "mbr",
                count: 5,
                maximum: 4
            }
        );
    }

    #[test]
    fn missing_type_names_the_partition() {
        let layout = resolve(
            512,
            vec![
                table_partition(),
                partition("blob", Role::Custom, Some(0x100000), 0x100000),
            ],
        );
        let error = write(&layout).unwrap_err();
        assert_eq!(
            error,
            TableError::MissingAttribute {
                partition: "blob".into(),
                writer: "mbr",
                attribute: "type"
            }
        );
        assert!(error.to_string().contains("blob"), "{error}");
    }

    #[test]
    fn unaligned_offset() {
        let mut blob = typed(
            partition("blob", Role::Custom, Some(0x100100), 0x100000),
            Some(0x83),
            false,
        );
        blob.alignment = Some(256);
        let layout = resolve(256, vec![table_partition(), blob]);
        let error = write(&layout).unwrap_err();
        assert_eq!(
            error,
            TableError::Misaligned {
                partition: "blob".into(),
                writer: "mbr",
                what: "offset",
                value: 0x100100,
                required: 512
            }
        );
        let message = error.to_string();
        assert!(
            message.contains("offset 0x100100 is not a multiple of 0x200"),
            "{message}"
        );
    }

    #[test]
    fn unaligned_size_and_entry_in_the_first_sector() {
        let layout = resolve(
            256,
            vec![
                table_partition(),
                typed(
                    partition("blob", Role::Custom, Some(0x100000), 0x100100),
                    Some(0x83),
                    false,
                ),
            ],
        );
        let error = write(&layout).unwrap_err();
        assert_eq!(
            error,
            TableError::Misaligned {
                partition: "blob".into(),
                writer: "mbr",
                what: "size",
                value: 0x100100,
                required: 512
            }
        );
        let message = error.to_string();
        assert!(
            message.contains("size 0x100100 is not a multiple of 0x200"),
            "{message}"
        );
        let layout = resolve(
            256,
            vec![typed(
                partition("early", Role::Custom, Some(0), 0x1000),
                Some(0x83),
                false,
            )],
        );
        let error = write(&layout).unwrap_err();
        assert_eq!(
            error,
            TableError::CoversFirstSector {
                partition: "early".into(),
                writer: "mbr",
                offset: 0,
                minimum: 512
            }
        );
        let message = error.to_string();
        assert!(
            message.contains("offset 0x0 is below 0x200") && message.contains("first sector"),
            "{message}"
        );
    }

    #[test]
    fn sectors_must_fit_thirty_two_bits() {
        let layout = Layout {
            flash: Flash {
                size: 4 << 40,
                alignment: 512,
                base_address: 0,
            },
            partitions: vec![
                table_partition(),
                typed(
                    partition("huge", Role::Custom, Some(3 << 40), 0x1000),
                    Some(0x83),
                    false,
                ),
            ],
        }
        .resolve()
        .unwrap();
        assert_eq!(
            write(&layout).unwrap_err(),
            TableError::OutOfRange {
                partition: "huge".into(),
                writer: "mbr"
            }
        );
    }
}
