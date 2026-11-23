//! The headroom of every partition, given the size of the images written to them.

use super::{LayoutError, LayoutErrors, ResolvedLayout, Role};
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// The size of the image destined to a partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSize {
    /// The name of the partition.
    pub partition: String,
    /// The size of the image in bytes.
    pub size: u64,
}

/// The space used and left in every partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Total size of the flash in bytes.
    pub flash_size: u64,
    /// The minimum flash size of a layout that detects it.
    #[serde(default)]
    pub minimum_flash_size: Option<u64>,
    /// The number of bytes of flash that no partition uses.
    pub unallocated: u64,
    /// One entry per partition, in file order.
    pub partitions: Vec<PartitionReport>,
}

/// The space used and left in one partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionReport {
    /// The name of the partition.
    pub name: String,
    /// What the partition is for.
    pub role: Role,
    /// Offset from the start of the flash.
    pub offset: u64,
    /// Exclusive end offset.
    pub end: u64,
    /// Size in bytes.
    pub size: u64,
    /// The least size, for a partition whose size depends on the flash.
    #[serde(default)]
    pub minimum_size: Option<u64>,
    /// The bytes above the minimum size, for such a partition.
    #[serde(default)]
    pub growth: Option<u64>,
    /// Whether its size depends on the flash of the device.
    #[serde(default)]
    pub grows_with_flash: bool,
    /// Whether its offset depends on the flash of the device.
    #[serde(default)]
    pub moves_with_flash: bool,
    /// The size of the image, if one was given.
    pub image_size: Option<u64>,
    /// The bytes left after the image, if one was given.
    pub headroom: Option<u64>,
    /// The headroom as a percentage of the partition size, rounded down.
    pub headroom_percent: Option<u8>,
}

impl ResolvedLayout {
    /// Compare the size of images with the size of their partitions.
    ///
    /// When several images are given for one partition, the largest counts. Every unknown
    /// partition and every image that does not fit is reported, not only the first.
    pub fn report(&self, images: &[ImageSize]) -> Result<Report, LayoutErrors> {
        let mut errors = Vec::new();
        for image in images {
            match self.partition(&image.partition) {
                None => errors.push(LayoutError::UnknownPartition {
                    name: image.partition.clone(),
                }),
                Some(partition) if image.size > partition.size => {
                    errors.push(LayoutError::ImageTooLarge {
                        partition: image.partition.clone(),
                        image_size: image.size,
                        partition_size: partition.size,
                    });
                }
                Some(_) => {}
            }
        }
        if !errors.is_empty() {
            return Err(LayoutErrors(errors));
        }

        let partitions = self
            .partitions
            .iter()
            .map(|partition| {
                let name = &partition.definition.name;
                let image_size = images
                    .iter()
                    .filter(|image| &image.partition == name)
                    .map(|image| image.size)
                    .max();
                let headroom = image_size.map(|image| partition.size.saturating_sub(image));
                let headroom_percent = headroom.map(|headroom| {
                    let percent = u128::from(headroom) * 100 / u128::from(partition.size.max(1));
                    u8::try_from(percent).unwrap_or(100)
                });
                let minimum_size = partition
                    .grows_with_flash
                    .then(|| partition.definition.size.minimum());
                PartitionReport {
                    name: name.clone(),
                    role: partition.definition.role,
                    offset: partition.offset,
                    end: partition.end(),
                    size: partition.size,
                    minimum_size,
                    growth: minimum_size.map(|minimum| partition.size.saturating_sub(minimum)),
                    grows_with_flash: partition.grows_with_flash,
                    moves_with_flash: partition.moves_with_flash,
                    image_size,
                    headroom,
                    headroom_percent,
                }
            })
            .collect();
        Ok(Report {
            flash_size: self.flash.size,
            minimum_flash_size: self.minimum_flash_size,
            unallocated: self.unallocated(),
            partitions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{
        Flash, GrownPartition, Layout, LayoutChanges, MovedPartition, Partition, Size,
    };
    use alloc::string::ToString;
    use alloc::vec;

    #[cfg(feature = "toml")]
    fn image(partition: &str, size: u64) -> ImageSize {
        ImageSize {
            partition: partition.to_string(),
            size,
        }
    }

    #[cfg(feature = "toml")]
    fn esp32() -> ResolvedLayout {
        Layout::from_toml(include_str!("../../tests/layouts/esp32-s3-8mb.toml"))
            .unwrap()
            .resolve()
            .unwrap()
    }

    #[cfg(feature = "toml")]
    #[test]
    fn reports_headroom() {
        let report = esp32().report(&[image("application", 0xC0000)]).unwrap();
        assert_eq!(report.flash_size, 0x800000);
        assert_eq!(report.unallocated, 0xF000);
        assert_eq!(report.partitions.len(), 8);
        for entry in &report.partitions {
            if entry.name == "application" {
                assert_eq!(entry.role, Role::Firmware);
                assert_eq!(entry.offset, 0x20000);
                assert_eq!(entry.end, 0x120000);
                assert_eq!(entry.size, 0x100000);
                assert_eq!(entry.image_size, Some(0xC0000));
                assert_eq!(entry.headroom, Some(0x40000));
                assert_eq!(entry.headroom_percent, Some(25));
            } else {
                assert_eq!(entry.image_size, None, "{}", entry.name);
                assert_eq!(entry.headroom, None, "{}", entry.name);
                assert_eq!(entry.headroom_percent, None, "{}", entry.name);
            }
        }
    }

    #[cfg(feature = "toml")]
    const LAYOUT_A: &str = r#"
[flash]
size = "detect"
minimum = "4 MiB"
alignment = "4 KiB"

[[partition]]
name = "bootloader"
role = "bootloader"
offset = 0x0
size = "32 KiB"

[[partition]]
name = "partition-table"
role = "partition-table"
offset = 0x8000
size = "4 KiB"

[[partition]]
name = "application"
role = "firmware"
alignment = "64 KiB"
size = "1 MiB"

[[partition]]
name = "data"
role = "custom"
size = { minimum = "1 MiB", grow = 1 }

[[partition]]
name = "storage"
role = "storage"
size = { minimum = "1 MiB", grow = 3 }

[[partition]]
name = "coredump"
role = "crash-record"
size = "64 KiB"
"#;

    #[cfg(feature = "toml")]
    fn layout_a(flash_size: Option<u64>) -> ResolvedLayout {
        Layout::from_toml(LAYOUT_A)
            .unwrap()
            .resolve_for(flash_size)
            .unwrap()
    }

    #[cfg(feature = "toml")]
    #[test]
    fn reports_minimum_and_growth() {
        let report = layout_a(Some(0x1000000)).report(&[]).unwrap();
        assert_eq!(report.minimum_flash_size, Some(0x400000));
        assert_eq!(report.flash_size, 0x1000000);
        let find = |name: &str| report.partitions.iter().find(|p| p.name == name).unwrap();
        let storage = find("storage");
        assert_eq!(storage.minimum_size, Some(0x100000));
        assert_eq!(storage.growth, Some(0x9A8000));
        assert!(storage.grows_with_flash && storage.moves_with_flash);
        let coredump = find("coredump");
        assert!(coredump.moves_with_flash && !coredump.grows_with_flash);
        assert_eq!((coredump.minimum_size, coredump.growth), (None, None));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn fixed_layouts_report_as_before() {
        let report = esp32().report(&[image("application", 0xC0000)]).unwrap();
        assert_eq!(report.minimum_flash_size, None);
        for entry in &report.partitions {
            assert_eq!((entry.minimum_size, entry.growth), (None, None));
            assert!(!entry.grows_with_flash && !entry.moves_with_flash);
        }
    }

    #[cfg(feature = "toml")]
    #[test]
    fn changes_list_what_grew_and_moved() {
        let minimum = layout_a(None);
        let changes = layout_a(Some(0x1000000)).changes_from(&minimum);
        let grown = |name: &str, from, to| GrownPartition {
            name: name.into(),
            size_at_minimum: from,
            size: to,
        };
        let moved = |name: &str, from, to| MovedPartition {
            name: name.into(),
            offset_at_minimum: from,
            offset: to,
        };
        assert_eq!(
            changes.grown,
            vec![
                grown("data", 0x138000, 0x438000),
                grown("storage", 0x1A8000, 0xAA8000)
            ]
        );
        assert_eq!(
            changes.moved,
            vec![
                moved("storage", 0x248000, 0x548000),
                moved("coredump", 0x3F0000, 0xFF0000)
            ]
        );
        assert!(!changes.is_empty());
        assert!(minimum.changes_from(&minimum).is_empty());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn percent_rounds_down_and_full_images_have_none_left() {
        let report = esp32()
            .report(&[image("application", 0x100000), image("nvs", 1)])
            .unwrap();
        let find = |name: &str| report.partitions.iter().find(|p| p.name == name).unwrap();
        assert_eq!(find("application").headroom, Some(0));
        assert_eq!(find("application").headroom_percent, Some(0));
        assert_eq!(find("nvs").headroom_percent, Some(99));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn image_too_large() {
        let errors = esp32()
            .report(&[image("application", 0x100001)])
            .unwrap_err();
        assert_eq!(
            errors.0,
            vec![LayoutError::ImageTooLarge {
                partition: "application".into(),
                image_size: 0x100001,
                partition_size: 0x100000
            }]
        );
        let text = errors.to_string();
        assert!(text.contains("0x100001"), "{text}");
        assert!(text.contains("application"), "{text}");
    }

    #[cfg(feature = "toml")]
    #[test]
    fn unknown_partition() {
        let errors = esp32().report(&[image("missing", 1)]).unwrap_err();
        assert_eq!(
            errors.0,
            vec![LayoutError::UnknownPartition {
                name: "missing".into()
            }]
        );
    }

    #[cfg(feature = "toml")]
    #[test]
    fn every_image_error_is_reported_at_once() {
        let errors = esp32()
            .report(&[
                image("missing", 1),
                image("application", 0x100001),
                image("nvs", 1),
            ])
            .unwrap_err();
        assert_eq!(errors.0.len(), 2);
    }

    #[test]
    fn unallocated_counts_gaps() {
        let layout = Layout {
            flash: Flash {
                size: 0xE000,
                alignment: 0x1000,
                detect: false,
                base_address: 0,
            },
            partitions: vec![part("a", Some(0), 0x4000), part("b", Some(0x5000), 0x7000)],
        };
        let report = layout.resolve().unwrap().report(&[]).unwrap();
        assert_eq!(report.unallocated, 0x3000);
    }

    fn part(name: &str, offset: Option<u64>, size: u64) -> Partition {
        Partition {
            name: name.to_string(),
            role: Role::Custom,
            offset,
            size: Size::Bytes(size),
            alignment: None,
            filesystem: None,
            espressif: None,
            mbr: None,
            linker: None,
        }
    }

    #[test]
    fn sizes_round_trip_through_json() {
        let grow = Size::Grow {
            minimum: 4096,
            grow: core::num::NonZeroU32::new(3).unwrap(),
            maximum: None,
        };
        let text = r#"{"grow":{"minimum":4096,"grow":3,"maximum":null}}"#;
        assert_eq!(serde_json::to_string(&grow).unwrap(), text);
        assert_eq!(serde_json::from_str::<Size>(text).unwrap(), grow);
    }

    #[test]
    fn wire_shapes_round_trip() {
        assert_eq!(
            serde_json::to_string(&Size::Bytes(4096)).unwrap(),
            r#"{"bytes":4096}"#
        );
        assert_eq!(
            serde_json::to_string(&Size::Remaining).unwrap(),
            r#""remaining""#
        );
        assert_eq!(
            serde_json::from_str::<Size>(r#"{"bytes":4096}"#).unwrap(),
            Size::Bytes(4096)
        );
        let image = ImageSize {
            partition: "application".into(),
            size: 7,
        };
        let text = serde_json::to_string(&image).unwrap();
        assert_eq!(serde_json::from_str::<ImageSize>(&text).unwrap(), image);
        let report = Report {
            flash_size: 0x1000,
            minimum_flash_size: Some(0x800),
            unallocated: 0x800,
            partitions: vec![PartitionReport {
                name: "application".into(),
                role: Role::Firmware,
                offset: 0,
                end: 0x800,
                size: 0x800,
                minimum_size: Some(0x400),
                growth: Some(0x400),
                grows_with_flash: true,
                moves_with_flash: false,
                image_size: Some(0x400),
                headroom: Some(0x400),
                headroom_percent: Some(50),
            }],
        };
        let text = serde_json::to_string(&report).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&text).unwrap(), report);
        let changes = LayoutChanges {
            grown: vec![GrownPartition {
                name: "data".into(),
                size_at_minimum: 1,
                size: 2,
            }],
            moved: vec![MovedPartition {
                name: "log".into(),
                offset_at_minimum: 3,
                offset: 4,
            }],
        };
        let text = serde_json::to_string(&changes).unwrap();
        assert_eq!(
            serde_json::from_str::<LayoutChanges>(&text).unwrap(),
            changes
        );
        let old = r#"{"flash_size":1,"unallocated":0,"partitions":[]}"#;
        assert_eq!(
            serde_json::from_str::<Report>(old)
                .unwrap()
                .minimum_flash_size,
            None
        );
        let error = crate::layout::ParseError {
            message: "bad".into(),
        };
        let text = serde_json::to_string(&error).unwrap();
        assert_eq!(
            serde_json::from_str::<crate::layout::ParseError>(&text).unwrap(),
            error
        );
    }
}
