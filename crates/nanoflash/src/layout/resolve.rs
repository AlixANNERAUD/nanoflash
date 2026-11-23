//! Resolution of a layout into absolute offsets and sizes, with the checks.

use super::{Flash, Layout, LayoutError, LayoutErrors, Partition, Role, Size};
use alloc::vec::Vec;

/// A layout with every offset and size computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLayout {
    /// The flash device.
    pub flash: Flash,
    /// The partitions, in file order.
    pub partitions: Vec<ResolvedPartition>,
}

/// A partition with its offset and size computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPartition {
    /// Offset from the start of the flash.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
    /// The alignment that applied to the offset.
    pub alignment: u64,
    /// The partition as written in the layout.
    pub definition: Partition,
}

impl ResolvedPartition {
    /// The exclusive end offset.
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }

    /// The address of the partition in the memory map, or `None` if it does not fit in 64 bits.
    pub fn address(&self, flash: &Flash) -> Option<u64> {
        flash.base_address.checked_add(self.offset)
    }
}

impl ResolvedLayout {
    /// The partition with this name.
    pub fn partition(&self, name: &str) -> Option<&ResolvedPartition> {
        self.partitions
            .iter()
            .find(|partition| partition.definition.name == name)
    }

    /// The number of bytes of flash that no partition uses.
    pub fn unallocated(&self) -> u64 {
        let used = self.partitions.iter().fold(0u64, |total, partition| {
            total.saturating_add(partition.size)
        });
        self.flash.size.saturating_sub(used)
    }
}

impl Layout {
    /// Compute the offset and size of every partition and check the layout.
    ///
    /// Every problem found is reported, not only the first.
    pub fn resolve(&self) -> Result<ResolvedLayout, LayoutErrors> {
        let mut errors = Vec::new();
        let flash = &self.flash;

        if self.partitions.is_empty() {
            errors.push(LayoutError::EmptyLayout);
        }
        if flash.size == 0 {
            errors.push(LayoutError::ZeroFlashSize);
        }
        if is_valid_alignment(flash.alignment) {
            if flash.size % flash.alignment != 0 {
                errors.push(LayoutError::FlashSizeNotAligned {
                    size: flash.size,
                    alignment: flash.alignment,
                });
            }
        } else {
            errors.push(LayoutError::InvalidAlignment {
                partition: None,
                alignment: flash.alignment,
            });
        }

        check_names_and_slots(&self.partitions, &mut errors);

        let mut resolved: Vec<ResolvedPartition> = Vec::with_capacity(self.partitions.len());
        let mut cursor = 0u64;
        for (index, partition) in self.partitions.iter().enumerate() {
            let alignment = match partition.alignment {
                Some(own) if !is_valid_alignment(own) => {
                    errors.push(LayoutError::InvalidAlignment {
                        partition: Some(partition.name.clone()),
                        alignment: own,
                    });
                    1
                }
                Some(own) => own,
                // An invalid flash default is already reported.
                None if !is_valid_alignment(flash.alignment) => 1,
                None => flash.alignment,
            };

            let offset = match partition.offset {
                Some(offset) => {
                    if offset % alignment != 0 {
                        errors.push(LayoutError::MisalignedOffset {
                            partition: partition.name.clone(),
                            offset,
                            alignment,
                        });
                    }
                    offset
                }
                None => match align_up(cursor, alignment) {
                    Some(offset) => offset,
                    None => {
                        errors.push(LayoutError::Overflow {
                            partition: partition.name.clone(),
                        });
                        continue;
                    }
                },
            };

            let size = match partition.size {
                Size::Bytes(0) => {
                    errors.push(LayoutError::ZeroSize {
                        partition: partition.name.clone(),
                    });
                    continue;
                }
                Size::Bytes(bytes) => bytes,
                Size::Remaining => {
                    let limit = match self.partitions.get(index + 1) {
                        Some(next) => next.offset,
                        None => Some(flash.size),
                    };
                    match limit.and_then(|limit| limit.checked_sub(offset)) {
                        Some(size) if size > 0 => size,
                        _ => {
                            errors.push(LayoutError::NoSpaceForRemaining {
                                partition: partition.name.clone(),
                                offset,
                            });
                            continue;
                        }
                    }
                }
            };

            match offset.checked_add(size) {
                Some(end) => cursor = end,
                None => {
                    errors.push(LayoutError::Overflow {
                        partition: partition.name.clone(),
                    });
                    continue;
                }
            }
            resolved.push(ResolvedPartition {
                offset,
                size,
                alignment,
                definition: partition.clone(),
            });
        }

        let mut by_offset: Vec<&ResolvedPartition> = resolved.iter().collect();
        by_offset.sort_by_key(|partition| partition.offset);
        for pair in by_offset.windows(2) {
            if let [first, second] = pair {
                if first.end() > second.offset {
                    errors.push(LayoutError::Overlap {
                        first: first.definition.name.clone(),
                        second: second.definition.name.clone(),
                        first_end: first.end(),
                        second_offset: second.offset,
                    });
                }
            }
        }
        for partition in &resolved {
            if partition.end() > flash.size {
                errors.push(LayoutError::ExceedsFlash {
                    partition: partition.definition.name.clone(),
                    end: partition.end(),
                    flash_size: flash.size,
                });
            }
        }

        if errors.is_empty() {
            Ok(ResolvedLayout {
                flash: flash.clone(),
                partitions: resolved,
            })
        } else {
            Err(LayoutErrors(errors))
        }
    }
}

fn is_valid_alignment(alignment: u64) -> bool {
    alignment.is_power_of_two()
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let mask = alignment.checked_sub(1)?;
    Some(value.checked_add(mask)? & !mask)
}

fn check_names_and_slots(partitions: &[Partition], errors: &mut Vec<LayoutError>) {
    let mut first_remaining: Option<&Partition> = None;
    for (index, partition) in partitions.iter().enumerate() {
        if partition.name.is_empty() {
            errors.push(LayoutError::EmptyName);
        }
        let earlier = partitions.iter().take(index);
        // Reported once per name, at its second occurrence.
        if earlier
            .clone()
            .filter(|other| other.name == partition.name)
            .count()
            == 1
        {
            errors.push(LayoutError::DuplicateName {
                name: partition.name.clone(),
            });
        }
        if let Role::Update { slot } = partition.role {
            let clash = earlier
                .clone()
                .find(|other| other.role == Role::Update { slot });
            if let Some(first) = clash {
                errors.push(LayoutError::DuplicateSlot {
                    slot,
                    first: first.name.clone(),
                    second: partition.name.clone(),
                });
            }
        }
        if partition.size == Size::Remaining {
            match first_remaining {
                Some(first) => errors.push(LayoutError::SeveralRemaining {
                    first: first.name.clone(),
                    second: partition.name.clone(),
                }),
                None => first_remaining = Some(partition),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;
    use proptest::prelude::*;

    fn flash(size: u64, alignment: u64) -> Flash {
        Flash {
            size,
            alignment,
            base_address: 0,
        }
    }

    fn partition(name: &str, offset: Option<u64>, size: Size) -> Partition {
        Partition {
            name: name.to_string(),
            role: Role::Custom,
            offset,
            size,
            alignment: None,
            filesystem: None,
            espressif: None,
            mbr: None,
            linker: None,
        }
    }

    fn errors_of(layout: &Layout) -> Vec<LayoutError> {
        layout.resolve().unwrap_err().0
    }

    #[cfg(feature = "toml")]
    #[test]
    fn resolves_the_esp32_fixture() {
        let layout =
            Layout::from_toml(include_str!("../../tests/layouts/esp32-s3-8mb.toml")).unwrap();
        let resolved = layout.resolve().unwrap();
        let expected = [
            ("bootloader", 0x0, 0x8000),
            ("partition-table", 0x8000, 0x1000),
            ("nvs", 0x9000, 0x6000),
            ("otadata", 0xF000, 0x2000),
            ("application", 0x20000, 0x100000),
            ("ota-0", 0x120000, 0x100000),
            ("ota-1", 0x220000, 0x100000),
            ("storage", 0x320000, 0x800000 - 0x320000),
        ];
        assert_eq!(resolved.partitions.len(), expected.len());
        for (partition, (name, offset, size)) in resolved.partitions.iter().zip(expected) {
            assert_eq!(partition.definition.name, name);
            assert_eq!(partition.offset, offset, "{name}");
            assert_eq!(partition.size, size, "{name}");
        }
        let storage = resolved.partition("storage").unwrap();
        assert_eq!(storage.end(), 0x800000);
        assert_eq!(resolved.unallocated(), 0xF000);
        assert_eq!(
            resolved.partition("application").unwrap().alignment,
            0x10000
        );
        assert!(resolved.partition("missing").is_none());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn remaining_stops_at_the_next_fixed_partition() {
        let layout =
            Layout::from_toml(include_str!("../../tests/layouts/nrf52840-arduino.toml")).unwrap();
        let resolved = layout.resolve().unwrap();
        let application = resolved.partition("application").unwrap();
        assert_eq!(application.offset, 0x10000);
        assert_eq!(application.end(), 0xF0000);
        assert_eq!(resolved.unallocated(), 0);
    }

    #[test]
    fn addresses_add_the_base_address() {
        let mut layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![partition("a", Some(0x1000), Size::Bytes(0x1000))],
        };
        layout.flash.base_address = 0x0800_0000;
        let resolved = layout.resolve().unwrap();
        let a = resolved.partition("a").unwrap();
        assert_eq!(a.address(&resolved.flash), Some(0x0800_1000));
        let far = Flash {
            base_address: u64::MAX,
            ..resolved.flash
        };
        assert_eq!(a.address(&far), None);
        assert_eq!(resolved.unallocated(), 0xF000);
    }

    #[test]
    fn reports_every_error_at_once() {
        let layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("a", Some(0x0), Size::Bytes(0x2000)),
                partition("a", Some(0x1000), Size::Bytes(0x1000)),
                partition("b", Some(0xF000), Size::Bytes(0x2000)),
            ],
        };
        let errors = errors_of(&layout);
        assert_eq!(
            errors,
            vec![
                LayoutError::DuplicateName { name: "a".into() },
                LayoutError::Overlap {
                    first: "a".into(),
                    second: "a".into(),
                    first_end: 0x2000,
                    second_offset: 0x1000
                },
                LayoutError::ExceedsFlash {
                    partition: "b".into(),
                    end: 0x11000,
                    flash_size: 0x10000
                },
            ]
        );
        let text = LayoutErrors(errors).to_string();
        assert_eq!(text.lines().count(), 3);
        assert!(text.contains("0x11000"), "{text}");
    }

    #[test]
    fn overlap_message_names_both_partitions() {
        let layout = Layout {
            flash: flash(0x20000, 0x1000),
            partitions: vec![
                partition("nvs", Some(0x9000), Size::Bytes(0x7000)),
                partition("application", Some(0xF000), Size::Bytes(0x1000)),
            ],
        };
        assert_eq!(
            LayoutErrors(errors_of(&layout)).to_string(),
            "partitions \"nvs\" and \"application\" overlap: \"nvs\" ends at 0x10000 but \"application\" starts at 0xf000"
        );
    }

    #[test]
    fn degenerate_layouts() {
        let empty = Layout {
            flash: flash(0x1000, 0x1000),
            partitions: vec![],
        };
        assert_eq!(errors_of(&empty), vec![LayoutError::EmptyLayout]);

        let a = partition("a", None, Size::Bytes(0x1000));
        let zero = Layout {
            flash: flash(0, 0x1000),
            partitions: vec![a.clone()],
        };
        assert!(errors_of(&zero).contains(&LayoutError::ZeroFlashSize));

        let bad = Layout {
            flash: flash(0x1000, 3),
            partitions: vec![a.clone()],
        };
        assert!(errors_of(&bad).contains(&LayoutError::InvalidAlignment {
            partition: None,
            alignment: 3
        }));

        let unaligned = Layout {
            flash: flash(0x1800, 0x1000),
            partitions: vec![a.clone()],
        };
        assert_eq!(
            errors_of(&unaligned),
            vec![LayoutError::FlashSizeNotAligned {
                size: 0x1800,
                alignment: 0x1000
            }]
        );

        let mut own = a.clone();
        own.alignment = Some(0);
        let zero_alignment = Layout {
            flash: flash(0x1000, 0x1000),
            partitions: vec![own],
        };
        assert_eq!(
            errors_of(&zero_alignment),
            vec![LayoutError::InvalidAlignment {
                partition: Some("a".into()),
                alignment: 0
            }]
        );

        let named = Layout {
            flash: flash(0x4000, 0x1000),
            partitions: vec![
                partition("", None, Size::Bytes(0x1000)),
                partition("z", None, Size::Bytes(0)),
            ],
        };
        let errors = errors_of(&named);
        assert!(errors.contains(&LayoutError::EmptyName));
        assert!(errors.contains(&LayoutError::ZeroSize {
            partition: "z".into()
        }));
    }

    #[test]
    fn remaining_errors() {
        let layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("rest", Some(0x1000), Size::Remaining),
                partition("next", None, Size::Bytes(0x1000)),
            ],
        };
        assert_eq!(
            errors_of(&layout),
            vec![LayoutError::NoSpaceForRemaining {
                partition: "rest".into(),
                offset: 0x1000
            }]
        );

        let at_end = Layout {
            flash: flash(0x2000, 0x1000),
            partitions: vec![
                partition("a", None, Size::Bytes(0x2000)),
                partition("rest", None, Size::Remaining),
            ],
        };
        assert_eq!(
            errors_of(&at_end),
            vec![LayoutError::NoSpaceForRemaining {
                partition: "rest".into(),
                offset: 0x2000
            }]
        );

        let two = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("a", Some(0), Size::Remaining),
                partition("b", Some(0x8000), Size::Remaining),
            ],
        };
        assert!(errors_of(&two).contains(&LayoutError::SeveralRemaining {
            first: "a".into(),
            second: "b".into()
        }));
    }

    #[test]
    fn misaligned_fixed_offset() {
        let layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![partition("nvs", Some(0x9001), Size::Bytes(0x1000))],
        };
        let errors = errors_of(&layout);
        assert_eq!(
            errors,
            vec![LayoutError::MisalignedOffset {
                partition: "nvs".into(),
                offset: 0x9001,
                alignment: 0x1000
            }]
        );
        assert!(LayoutErrors(errors).to_string().contains("0x9001"));
    }

    #[test]
    fn duplicate_update_slots() {
        let mut first = partition("ota-a", None, Size::Bytes(0x1000));
        first.role = Role::Update { slot: 0 };
        let mut second = partition("ota-b", None, Size::Bytes(0x1000));
        second.role = Role::Update { slot: 0 };
        let layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![first, second],
        };
        assert_eq!(
            errors_of(&layout),
            vec![LayoutError::DuplicateSlot {
                slot: 0,
                first: "ota-a".into(),
                second: "ota-b".into()
            }]
        );
    }

    #[test]
    fn overflow_is_reported() {
        let layout = Layout {
            flash: flash(0x1000, 0x1000),
            partitions: vec![
                partition("a", Some(u64::MAX - 0xFFF), Size::Bytes(0x1000)),
                partition("b", None, Size::Bytes(1)),
            ],
        };
        let errors = errors_of(&layout);
        assert!(errors.contains(&LayoutError::Overflow {
            partition: "a".into()
        }));
    }

    #[test]
    fn errors_serialize_with_a_kind() {
        let error = LayoutError::DuplicateName { name: "x".into() };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"kind":"duplicate-name","name":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&LayoutError::EmptyLayout).unwrap(),
            r#"{"kind":"empty-layout"}"#
        );
    }

    fn arbitrary_layout() -> impl Strategy<Value = Layout> {
        let alignment = (0u32..=16).prop_map(|shift| 1u64 << shift);
        let flash_size = prop_oneof![4 => 0u64..0x400000, 1 => any::<u64>()];
        let size = prop_oneof![
            4 => (1u64..0x40).prop_map(|units| Size::Bytes(units * 0x1000)),
            1 => any::<u64>().prop_map(Size::Bytes),
            1 => Just(Size::Remaining),
        ];
        let offset = prop_oneof![
            3 => Just(None),
            2 => (0u64..0x100).prop_map(|units| Some(units * 0x1000)),
            1 => any::<u64>().prop_map(Some),
        ];
        let partition_alignment = prop_oneof![
            4 => Just(None),
            2 => (0u32..=16).prop_map(|shift| Some(1u64 << shift)),
            1 => any::<u64>().prop_map(Some),
        ];
        (
            alignment,
            flash_size,
            any::<u64>(),
            proptest::collection::vec((offset, size, partition_alignment), 1..12),
        )
            .prop_map(|(alignment, size, base_address, parts)| Layout {
                flash: Flash {
                    size: size - size % alignment,
                    alignment,
                    base_address,
                },
                partitions: parts
                    .into_iter()
                    .enumerate()
                    .map(|(index, (offset, size, alignment))| {
                        let mut partition = partition(&format!("p{index}"), offset, size);
                        partition.alignment = alignment;
                        partition
                    })
                    .collect(),
            })
    }

    proptest! {
        #[test]
        fn resolution_never_panics_and_ok_means_sound(layout in arbitrary_layout()) {
            if let Ok(resolved) = layout.resolve() {
                prop_assert_eq!(resolved.partitions.len(), layout.partitions.len());
                let mut spans = Vec::new();
                for (partition, source) in resolved.partitions.iter().zip(&layout.partitions) {
                    prop_assert_eq!(&partition.definition, source);
                    prop_assert!(partition.alignment.is_power_of_two());
                    prop_assert_eq!(partition.offset % partition.alignment, 0);
                    prop_assert!(partition.size > 0);
                    prop_assert!(partition.end() <= layout.flash.size);
                    if let Some(fixed) = source.offset {
                        prop_assert_eq!(partition.offset, fixed);
                    }
                    spans.push((partition.offset, partition.end()));
                }
                spans.sort();
                for pair in spans.windows(2) {
                    prop_assert!(pair[0].1 <= pair[1].0);
                }
                prop_assert!(resolved.unallocated() <= layout.flash.size);
            }
        }
    }
}
