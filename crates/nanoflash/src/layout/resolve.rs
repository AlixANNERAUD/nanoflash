//! Resolution of a layout into absolute offsets and sizes, with the checks.
//!
//! Partitions with a fixed offset split the file order into segments, each ending where the next
//! one starts or, for the last, at the end of the flash. A first pass places every partition at
//! its minimum size; the free space left at the end of each segment is then shared among its
//! growing partitions (`remaining` and `{ grow }`), and a second pass places them again with
//! their share. A share is a multiple of the largest alignment of the partitions after it in the
//! segment, so these move by whole multiples of their alignment and need no new padding.

use super::growth::{Claim, distribute, floor_to};
use super::{Flash, Layout, LayoutError, LayoutErrors, Partition, Role, Size};
use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroU32;

/// A layout with every offset and size computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLayout {
    /// The flash device. With `detect`, `size` is the size resolved against.
    pub flash: Flash,
    /// The minimum flash size of a `detect` layout, `None` for a fixed size.
    pub minimum_flash_size: Option<u64>,
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
    /// Whether its size depends on the flash of the device (`detect` layouts only).
    pub grows_with_flash: bool,
    /// Whether its offset depends on the flash of the device (`detect` layouts only).
    pub moves_with_flash: bool,
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

/// What the resolution knows of one partition.
struct Plan {
    alignment: u64,
    /// The share it asks of the free space of its segment, when it grows.
    claim: Option<Claim>,
    /// Its offset and end in the first pass, `None` when it could not be placed.
    first: Option<(u64, u64)>,
    /// Its share of the free space.
    growth: u64,
    grows_with_flash: bool,
    moves_with_flash: bool,
}

/// A run of partitions in file order that starts at a fixed offset (or at the first partition).
struct Segment {
    /// The index of its first partition.
    start: usize,
    /// The index after its last partition.
    end: usize,
    /// Where its space ends.
    limit: u64,
    /// Where its free space starts, after the first pass.
    free_from: u64,
    /// Whether every partition of it was placed and its free space shared.
    shared: bool,
}

impl Layout {
    /// Compute the offset and size of every partition and check the layout, for the flash size
    /// of the layout (the minimum of a `detect` layout).
    ///
    /// Every problem found is reported, not only the first.
    pub fn resolve(&self) -> Result<ResolvedLayout, LayoutErrors> {
        self.resolve_for(None)
    }

    /// Compute the offset and size of every partition and check the layout, for a flash of
    /// `flash_size` bytes (only a `detect` layout, and not below its minimum), or for the flash
    /// size of the layout when `None`.
    ///
    /// Every problem found is reported, not only the first.
    pub fn resolve_for(&self, flash_size: Option<u64>) -> Result<ResolvedLayout, LayoutErrors> {
        let flash = &self.flash;
        let size = match flash_size {
            None => flash.size,
            Some(requested) if !flash.detect => {
                return Err(LayoutErrors(vec![LayoutError::FlashSizeFixed {
                    size: flash.size,
                    requested,
                }]));
            }
            Some(requested) if requested < flash.size => {
                return Err(LayoutErrors(vec![LayoutError::FlashBelowMinimum {
                    size: requested,
                    minimum: flash.size,
                }]));
            }
            Some(requested) => requested,
        };
        let mut errors = Vec::new();

        if self.partitions.is_empty() {
            errors.push(LayoutError::EmptyLayout);
        }
        if size == 0 {
            errors.push(LayoutError::ZeroFlashSize);
        }
        if is_valid_alignment(flash.alignment) {
            if size % flash.alignment != 0 {
                errors.push(LayoutError::FlashSizeNotAligned {
                    size,
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

        let mut plans = Vec::with_capacity(self.partitions.len());
        let mut cursor = 0u64;
        for partition in &self.partitions {
            let alignment = alignment_of(partition, flash, &mut errors);
            let first = place_at_minimum(partition, alignment, cursor, &mut errors);
            if let Some((_, end)) = first {
                cursor = end;
            }
            plans.push(Plan {
                alignment,
                claim: None,
                first,
                growth: 0,
                grows_with_flash: false,
                moves_with_flash: false,
            });
        }

        let mut segments = segments_of(&self.partitions, size);
        for segment in &mut segments {
            share_free_space(&self.partitions, &mut plans, segment, &mut errors);
        }

        let mut flag_errors = Vec::new();
        if flash.detect {
            if let Some(last) = segments.last() {
                flag_dependence(&self.partitions, &mut plans, last, &mut flag_errors);
            }
        }

        let mut resolved: Vec<ResolvedPartition> = Vec::with_capacity(self.partitions.len());
        let mut cursor = 0u64;
        for (index, (partition, plan)) in self.partitions.iter().zip(&plans).enumerate() {
            // A partition the first pass could not place is already reported.
            if plan.first.is_none() {
                continue;
            }
            let offset = match partition.offset {
                Some(offset) => offset,
                None => match align_up(cursor, plan.alignment) {
                    Some(offset) => offset,
                    None => {
                        errors.push(LayoutError::Overflow {
                            partition: partition.name.clone(),
                        });
                        continue;
                    }
                },
            };
            let Some(size) = partition.size.minimum().checked_add(plan.growth) else {
                errors.push(LayoutError::Overflow {
                    partition: partition.name.clone(),
                });
                continue;
            };
            if size == 0 {
                // A `remaining` without a share.
                let segment = segments
                    .iter()
                    .find(|segment| (segment.start..segment.end).contains(&index));
                if let Some(segment) = segment.filter(|segment| segment.shared) {
                    errors.push(no_space_error(
                        &self.partitions,
                        partition,
                        offset,
                        segment,
                        index,
                        plan,
                    ));
                }
                cursor = offset;
                continue;
            }
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
                alignment: plan.alignment,
                grows_with_flash: plan.grows_with_flash,
                moves_with_flash: plan.moves_with_flash,
                definition: partition.clone(),
            });
        }
        errors.append(&mut flag_errors);

        let mut by_offset: Vec<&ResolvedPartition> = resolved.iter().collect();
        by_offset.sort_by_key(|partition| partition.offset);
        // Every partition is compared with all the earlier ones, not only its neighbour: a long
        // partition can overlap several later ones.
        for (index, second) in by_offset.iter().enumerate() {
            for first in by_offset.iter().take(index) {
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
            if partition.end() > size {
                errors.push(LayoutError::ExceedsFlash {
                    partition: partition.definition.name.clone(),
                    end: partition.end(),
                    flash_size: size,
                });
            }
        }

        if errors.is_empty() {
            Ok(ResolvedLayout {
                flash: Flash {
                    size,
                    ..flash.clone()
                },
                minimum_flash_size: flash.detect.then_some(flash.size),
                partitions: resolved,
            })
        } else {
            Err(LayoutErrors(errors))
        }
    }
}

/// The alignment of a partition: its own, or the flash default, or 1 when that is invalid.
fn alignment_of(partition: &Partition, flash: &Flash, errors: &mut Vec<LayoutError>) -> u64 {
    match partition.alignment {
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
    }
}

/// The offset and end of a partition at its minimum size after `cursor`, with the checks of its
/// offset and size, or `None` when it cannot be placed.
fn place_at_minimum(
    partition: &Partition,
    alignment: u64,
    cursor: u64,
    errors: &mut Vec<LayoutError>,
) -> Option<(u64, u64)> {
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
                return None;
            }
        },
    };

    let mut placed = true;
    match partition.size {
        Size::Bytes(0) | Size::Grow { minimum: 0, .. } => {
            errors.push(LayoutError::ZeroSize {
                partition: partition.name.clone(),
            });
            placed = false;
        }
        Size::Grow {
            minimum,
            maximum: Some(maximum),
            ..
        } if maximum < minimum => {
            errors.push(LayoutError::InvalidGrowth {
                partition: partition.name.clone(),
                minimum,
                maximum,
            });
            placed = false;
        }
        _ => {}
    }
    if matches!(partition.size, Size::Grow { .. }) && needs_fixed_size(partition.role) {
        errors.push(LayoutError::RoleCannotGrow {
            partition: partition.name.clone(),
            role: partition.role,
        });
    }
    if !placed {
        return None;
    }

    match offset.checked_add(partition.size.minimum()) {
        Some(end) => Some((offset, end)),
        None => {
            errors.push(LayoutError::Overflow {
                partition: partition.name.clone(),
            });
            None
        }
    }
}

/// The segments of the file order, each with the offset where its space ends.
fn segments_of(partitions: &[Partition], flash_size: u64) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for (index, partition) in partitions.iter().enumerate() {
        if index == 0 || partition.offset.is_some() {
            if let Some(previous) = segments.last_mut() {
                previous.end = index;
                previous.limit = partition.offset.unwrap_or(flash_size);
            }
            segments.push(Segment {
                start: index,
                end: partitions.len(),
                limit: flash_size,
                free_from: 0,
                shared: false,
            });
        }
    }
    segments
}

/// Set the claims of the growing partitions of a segment and share its free space among them.
fn share_free_space(
    partitions: &[Partition],
    plans: &mut [Plan],
    segment: &mut Segment,
    errors: &mut Vec<LayoutError>,
) {
    let (Some(partitions), Some(plans)) = (
        partitions.get(segment.start..segment.end),
        plans.get_mut(segment.start..segment.end),
    ) else {
        return;
    };

    // From the last partition back, so that `after` is the largest alignment of those after it.
    let mut after = 1u64;
    for (partition, plan) in partitions.iter().zip(plans.iter_mut()).rev() {
        plan.claim = match partition.size {
            Size::Bytes(_) => None,
            Size::Remaining => Some(Claim {
                weight: NonZeroU32::MIN,
                unit: after,
                cap: None,
            }),
            Size::Grow {
                minimum,
                grow,
                maximum,
            } => {
                let unit = plan.alignment.max(after);
                Some(Claim {
                    weight: grow,
                    unit,
                    cap: maximum.map(|maximum| floor_to(maximum.saturating_sub(minimum), unit)),
                })
            }
        };
        after = after.max(plan.alignment);
    }

    let mut end = segment.limit;
    for plan in plans.iter() {
        match plan.first {
            Some((_, first_end)) => end = first_end,
            None => return,
        }
    }
    segment.free_from = end;
    let extra = segment.limit.saturating_sub(end);

    let claims: Vec<Claim> = plans.iter().filter_map(|plan| plan.claim).collect();
    let Some(shares) = distribute(extra, &claims) else {
        if let Some((partition, _)) = partitions
            .iter()
            .zip(plans.iter())
            .find(|(_, plan)| plan.claim.is_some())
        {
            errors.push(LayoutError::Overflow {
                partition: partition.name.clone(),
            });
        }
        return;
    };
    let growing = plans.iter_mut().filter(|plan| plan.claim.is_some());
    for (plan, share) in growing.zip(shares) {
        plan.growth = share;
    }
    segment.shared = true;
}

/// Flag the partitions of the last segment of a `detect` layout whose size or offset depends on
/// the flash of the device, and refuse the roles that need them fixed.
fn flag_dependence(
    partitions: &[Partition],
    plans: &mut [Plan],
    last: &Segment,
    errors: &mut Vec<LayoutError>,
) {
    let (Some(partitions), Some(plans)) = (
        partitions.get(last.start..last.end),
        plans.get_mut(last.start..last.end),
    ) else {
        return;
    };
    let mut first_growing: Option<&Partition> = None;
    for (partition, plan) in partitions.iter().zip(plans.iter_mut()) {
        // A `{ grow }` held to its minimum by its maximum never grows.
        plan.grows_with_flash = plan.claim.is_some_and(|claim| claim.cap != Some(0));
        plan.moves_with_flash = partition.offset.is_none() && first_growing.is_some();
        // A `{ grow }` of these roles is refused whatever the layout.
        if plan.grows_with_flash
            && partition.size == Size::Remaining
            && needs_fixed_size(partition.role)
        {
            errors.push(LayoutError::RoleCannotGrow {
                partition: partition.name.clone(),
                role: partition.role,
            });
        }
        if let Some(after) = first_growing.filter(|_| plan.moves_with_flash) {
            if needs_fixed_offset(partition.role) {
                errors.push(LayoutError::OffsetDependsOnFlash {
                    partition: partition.name.clone(),
                    after: after.name.clone(),
                });
            }
        }
        if plan.grows_with_flash && first_growing.is_none() {
            first_growing = Some(partition);
        }
    }
}

/// The error of a `remaining` partition left without space.
fn no_space_error(
    partitions: &[Partition],
    partition: &Partition,
    offset: u64,
    segment: &Segment,
    index: usize,
    plan: &Plan,
) -> LayoutError {
    let last = index.checked_add(1) == Some(segment.end);
    if last && segment.free_from >= segment.limit {
        LayoutError::NoSpaceForRemaining {
            partition: partition.name.clone(),
            offset,
            limit: segment.limit,
            limited_by: partitions.get(segment.end).map(|next| next.name.clone()),
        }
    } else {
        LayoutError::NoShareForRemaining {
            partition: partition.name.clone(),
            start: segment.free_from,
            limit: segment.limit,
            step: plan.claim.map_or(1, |claim| claim.unit),
        }
    }
}

/// Roles whose image is built for a size.
fn needs_fixed_size(role: Role) -> bool {
    matches!(role, Role::Firmware | Role::Update { .. })
}

/// Roles whose offset is built into the bootloader, the table or the image.
fn needs_fixed_offset(role: Role) -> bool {
    matches!(
        role,
        Role::Bootloader | Role::PartitionTable | Role::Firmware | Role::Update { .. }
    )
}

fn is_valid_alignment(alignment: u64) -> bool {
    alignment.is_power_of_two()
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let mask = alignment.checked_sub(1)?;
    Some(value.checked_add(mask)? & !mask)
}

fn check_names_and_slots(partitions: &[Partition], errors: &mut Vec<LayoutError>) {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::legacy;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;
    use proptest::prelude::*;

    fn flash(size: u64, alignment: u64) -> Flash {
        Flash {
            size,
            alignment,
            detect: false,
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
            detect: false,
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
    fn a_long_partition_overlaps_every_later_one() {
        let layout = Layout {
            flash: flash(0x20000, 0x1000),
            partitions: vec![
                partition("a", Some(0x0), Size::Bytes(0x10000)),
                partition("b", Some(0x1000), Size::Bytes(0x1000)),
                partition("c", Some(0x3000), Size::Bytes(0x1000)),
            ],
        };
        let pairs: Vec<(alloc::string::String, alloc::string::String)> = errors_of(&layout)
            .into_iter()
            .map(|error| match error {
                LayoutError::Overlap { first, second, .. } => (first, second),
                other => panic!("unexpected {other}"),
            })
            .collect();
        assert_eq!(
            pairs,
            vec![("a".into(), "b".into()), ("a".into(), "c".into())]
        );
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
        // Valid before nothing pushed a partition: `rest` now shares its segment with `next`.
        let layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("rest", Some(0x1000), Size::Remaining),
                partition("next", None, Size::Bytes(0x1000)),
            ],
        };
        let resolved = layout.resolve().unwrap();
        assert_eq!(spans(&resolved), [(0x1000, 0xE000), (0xF000, 0x1000)]);

        let squeezed = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("rest", Some(0x2000), Size::Remaining),
                partition("next", Some(0x2000), Size::Bytes(0x1000)),
            ],
        };
        assert_eq!(
            errors_of(&squeezed),
            vec![LayoutError::NoSpaceForRemaining {
                partition: "rest".into(),
                offset: 0x2000,
                limit: 0x2000,
                limited_by: Some("next".into())
            }]
        );
        assert_eq!(
            LayoutErrors(errors_of(&squeezed)).to_string(),
            "partition \"rest\" uses the remaining space but starts at 0x2000, and the next partition \"next\" starts at 0x2000, so no space is left"
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
                offset: 0x2000,
                limit: 0x2000,
                limited_by: None
            }]
        );
        assert_eq!(
            LayoutErrors(errors_of(&at_end)).to_string(),
            "partition \"rest\" uses the remaining space but starts at 0x2000, and the flash ends at 0x2000, so no space is left"
        );

        let two = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("a", Some(0), Size::Remaining),
                partition("b", Some(0x8000), Size::Remaining),
            ],
        };
        let resolved = two.resolve().unwrap();
        assert_eq!(spans(&resolved), [(0, 0x8000), (0x8000, 0x8000)]);

        // Space is left, but less than the alignment of the partition after `rest`.
        let rounded = Layout {
            flash: flash(0x20000, 0x1000),
            partitions: vec![
                partition("rest", Some(0x1000), Size::Remaining),
                aligned(partition("next", None, Size::Bytes(0x1000)), 0x10000),
            ],
        };
        assert_eq!(
            errors_of(&rounded),
            vec![LayoutError::NoShareForRemaining {
                partition: "rest".into(),
                start: 0x11000,
                limit: 0x20000,
                step: 0x10000,
            }]
        );
        assert_eq!(
            LayoutErrors(errors_of(&rounded)).to_string(),
            "partition \"rest\" uses the remaining space, but its share of the free space from 0x11000 to 0x20000 rounds down to zero bytes in steps of 0x10000"
        );
    }

    fn grow(minimum: u64, weight: u32, maximum: Option<u64>) -> Size {
        Size::Grow {
            minimum,
            grow: core::num::NonZeroU32::new(weight).unwrap(),
            maximum,
        }
    }

    fn aligned(mut partition: Partition, alignment: u64) -> Partition {
        partition.alignment = Some(alignment);
        partition
    }

    fn with_role(mut partition: Partition, role: Role) -> Partition {
        partition.role = role;
        partition
    }

    fn spans(resolved: &ResolvedLayout) -> Vec<(u64, u64)> {
        resolved
            .partitions
            .iter()
            .map(|partition| (partition.offset, partition.size))
            .collect()
    }

    fn flags(resolved: &ResolvedLayout) -> Vec<(&str, bool, bool)> {
        resolved
            .partitions
            .iter()
            .map(|partition| {
                (
                    partition.definition.name.as_str(),
                    partition.grows_with_flash,
                    partition.moves_with_flash,
                )
            })
            .collect()
    }

    fn at(resolved: &ResolvedLayout, name: &str) -> (u64, u64) {
        let partition = resolved.partition(name).unwrap();
        (partition.offset, partition.size)
    }

    /// Layout A: a `detect` flash of at least 4 MiB, two partitions growing by 1 and 3.
    fn layout_a() -> Layout {
        Layout {
            flash: Flash {
                size: 0x400000,
                alignment: 0x1000,
                detect: true,
                base_address: 0,
            },
            partitions: vec![
                with_role(
                    partition("bootloader", Some(0), Size::Bytes(0x8000)),
                    Role::Bootloader,
                ),
                with_role(
                    partition("partition-table", Some(0x8000), Size::Bytes(0x1000)),
                    Role::PartitionTable,
                ),
                aligned(
                    with_role(
                        partition("application", None, Size::Bytes(0x100000)),
                        Role::Firmware,
                    ),
                    0x10000,
                ),
                partition("data", None, grow(0x100000, 1, None)),
                with_role(
                    partition("storage", None, grow(0x100000, 3, None)),
                    Role::Storage,
                ),
                with_role(
                    partition("coredump", None, Size::Bytes(0x10000)),
                    Role::CrashRecord,
                ),
            ],
        }
    }

    #[test]
    fn grows_by_weight_on_the_minimum_and_on_a_larger_flash() {
        let layout = layout_a();
        let minimum = layout.resolve().unwrap();
        assert_eq!(minimum.flash.size, 0x400000);
        assert_eq!(minimum.minimum_flash_size, Some(0x400000));
        assert_eq!(at(&minimum, "application"), (0x10000, 0x100000));
        assert_eq!(at(&minimum, "data"), (0x110000, 0x138000));
        assert_eq!(at(&minimum, "storage"), (0x248000, 0x1A8000));
        assert_eq!(at(&minimum, "coredump"), (0x3F0000, 0x10000));
        assert_eq!(minimum.unallocated(), 0x7000);

        let larger = layout.resolve_for(Some(0x1000000)).unwrap();
        assert_eq!(larger.flash.size, 0x1000000);
        assert!(larger.flash.detect);
        assert_eq!(larger.minimum_flash_size, Some(0x400000));
        assert_eq!(at(&larger, "data"), (0x110000, 0x438000));
        assert_eq!(at(&larger, "storage"), (0x548000, 0xAA8000));
        assert_eq!(at(&larger, "coredump"), (0xFF0000, 0x10000));
        assert_eq!(larger.unallocated(), 0x7000);

        let expected = [
            ("bootloader", false, false),
            ("partition-table", false, false),
            ("application", false, false),
            ("data", true, false),
            ("storage", true, true),
            ("coredump", false, true),
        ];
        assert_eq!(flags(&minimum), expected);
        assert_eq!(flags(&larger), expected);
        assert_eq!(layout.resolve_for(Some(0x400000)), layout.resolve());
    }

    #[test]
    fn a_maximum_caps_a_partition() {
        let mut layout = layout_a();
        layout.partitions[3].size = grow(0x100000, 1, Some(0x200000));
        let minimum = layout.resolve().unwrap();
        assert_eq!(at(&minimum, "data"), (0x110000, 0x138000));
        let larger = layout.resolve_for(Some(0x1000000)).unwrap();
        assert_eq!(at(&larger, "data"), (0x110000, 0x200000));
        assert_eq!(at(&larger, "storage"), (0x310000, 0xCE0000));
        assert_eq!(at(&larger, "coredump"), (0xFF0000, 0x10000));
    }

    #[test]
    fn a_grow_that_cannot_grow_is_fixed_for_the_flags() {
        let mut layout = layout_a();
        layout.partitions[3].size = grow(0x100000, 1, Some(0x100000));
        let resolved = layout.resolve().unwrap();
        assert_eq!(at(&resolved, "data"), (0x110000, 0x100000));
        assert_eq!(at(&resolved, "storage"), (0x210000, 0x1E0000));
        assert_eq!(
            &flags(&resolved)[3..],
            [
                ("data", false, false),
                ("storage", true, false),
                ("coredump", false, true)
            ]
        );
        // A maximum less than one unit above the minimum cannot grow either.
        layout.partitions[3].size = grow(0x100000, 1, Some(0x100FFF));
        assert_eq!(flags(&layout.resolve().unwrap())[3], ("data", false, false));
        // So a firmware after it keeps its offset on every device.
        layout.partitions.swap(2, 3);
        let resolved = layout.resolve().unwrap();
        assert_eq!(flags(&resolved)[3], ("application", false, false));
    }

    #[test]
    fn a_later_alignment_sets_the_unit() {
        let layout = Layout {
            flash: flash(0xF8000, 0x1000),
            partitions: vec![
                partition("a", Some(0), Size::Bytes(0x1000)),
                partition("s", None, grow(0x1000, 1, None)),
                aligned(partition("f", None, Size::Bytes(0x10000)), 0x10000),
            ],
        };
        let resolved = layout.resolve().unwrap();
        assert_eq!(
            spans(&resolved),
            [(0, 0x1000), (0x1000, 0xD1000), (0xE0000, 0x10000)]
        );
        assert_eq!(resolved.unallocated(), 0x16000);
        assert_eq!(resolved.minimum_flash_size, None);
        assert!(
            flags(&resolved)
                .iter()
                .all(|(_, grows, moves)| !grows && !moves)
        );
    }

    #[test]
    fn remaining_fills_to_the_byte_as_before() {
        let layout = Layout {
            flash: flash(0x100000, 0x1000),
            partitions: vec![
                partition("a", Some(0), Size::Bytes(0x1000)),
                aligned(partition("rest", None, Size::Remaining), 0x10000),
                partition("next", Some(0xF1000), Size::Bytes(0x1000)),
            ],
        };
        let resolved = layout.resolve().unwrap();
        assert_eq!(at(&resolved, "rest"), (0x10000, 0xE1000));
    }

    #[test]
    fn remaining_pushes_the_partitions_after_it() {
        let layout = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("rest", Some(0x1000), Size::Remaining),
                partition("next", None, Size::Bytes(0x1000)),
            ],
        };
        let resolved = layout.resolve().unwrap();
        assert_eq!(at(&resolved, "rest"), (0x1000, 0xE000));
        assert_eq!(at(&resolved, "next"), (0xF000, 0x1000));
    }

    #[test]
    fn remaining_partitions_share_by_weight() {
        let one_segment = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("a", None, Size::Remaining),
                partition("b", None, Size::Remaining),
            ],
        };
        let resolved = one_segment.resolve().unwrap();
        assert_eq!(spans(&resolved), [(0, 0x8000), (0x8000, 0x8000)]);

        let two_segments = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![
                partition("a", Some(0), Size::Remaining),
                partition("b", Some(0x8000), Size::Remaining),
            ],
        };
        let resolved = two_segments.resolve().unwrap();
        assert_eq!(spans(&resolved), [(0, 0x8000), (0x8000, 0x8000)]);
    }

    #[test]
    fn growth_errors() {
        let zero = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![partition("a", None, grow(0, 1, None))],
        };
        assert!(errors_of(&zero).contains(&LayoutError::ZeroSize {
            partition: "a".into()
        }));

        let inverted = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![partition("a", None, grow(0x2000, 1, Some(0x1000)))],
        };
        assert_eq!(
            errors_of(&inverted),
            vec![LayoutError::InvalidGrowth {
                partition: "a".into(),
                minimum: 0x2000,
                maximum: 0x1000
            }]
        );
        assert_eq!(
            LayoutErrors(errors_of(&inverted)).to_string(),
            "partition \"a\" may grow to a maximum of 0x1000 bytes, less than its minimum of 0x2000 bytes"
        );

        for role in [Role::Firmware, Role::Update { slot: 0 }] {
            let firmware = Layout {
                flash: flash(0x10000, 0x1000),
                partitions: vec![with_role(
                    partition("application", None, grow(0x1000, 1, None)),
                    role,
                )],
            };
            assert_eq!(
                errors_of(&firmware),
                vec![LayoutError::RoleCannotGrow {
                    partition: "application".into(),
                    role
                }]
            );
        }
        let update = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![with_role(
                partition("ota-0", None, grow(0x1000, 1, None)),
                Role::Update { slot: 0 },
            )],
        };
        assert_eq!(
            LayoutErrors(errors_of(&update)).to_string(),
            "partition \"ota-0\" has role update and cannot grow: give it a fixed size"
        );
    }

    #[cfg(feature = "toml")]
    #[test]
    fn only_detect_layouts_depend_on_the_flash() {
        let fixed =
            Layout::from_toml(include_str!("../../tests/layouts/nrf52840-arduino.toml")).unwrap();
        let resolved = fixed.resolve().unwrap();
        assert_eq!(resolved.minimum_flash_size, None);
        assert!(
            flags(&resolved)
                .iter()
                .all(|(_, grows, moves)| !grows && !moves)
        );

        let mut detect = fixed.clone();
        detect.flash.detect = true;
        let detected = detect.resolve().unwrap();
        assert_eq!(spans(&detected), spans(&resolved));
        assert_eq!(detected.minimum_flash_size, Some(1 << 20));
        assert!(
            flags(&detected)
                .iter()
                .all(|(_, grows, moves)| !grows && !moves)
        );

        let mut last = detect.clone();
        last.partitions.truncate(2);
        assert_eq!(
            errors_of(&last),
            vec![LayoutError::RoleCannotGrow {
                partition: "application".into(),
                role: Role::Firmware
            }]
        );
        assert_eq!(
            LayoutErrors(errors_of(&last)).to_string(),
            "partition \"application\" has role firmware and cannot grow: give it a fixed size"
        );

        let mut after = layout_a();
        let application = after.partitions.remove(2);
        after.partitions.insert(4, application);
        assert_eq!(
            errors_of(&after),
            vec![LayoutError::OffsetDependsOnFlash {
                partition: "application".into(),
                after: "data".into()
            }]
        );
        assert_eq!(
            LayoutErrors(errors_of(&after)).to_string(),
            "partition \"application\" comes after \"data\", which grows with the flash of the device, so its offset would depend on the device: give \"application\" a fixed offset or put it before \"data\""
        );
        // The same partitions with a fixed flash size depend on nothing.
        after.flash.detect = false;
        assert!(after.resolve().is_ok());
    }

    #[test]
    fn resolving_for_a_flash_size() {
        let fixed = Layout {
            flash: flash(0x10000, 0x1000),
            partitions: vec![partition("a", None, Size::Bytes(0x1000))],
        };
        assert_eq!(
            fixed.resolve_for(Some(0x20000)).unwrap_err().0,
            vec![LayoutError::FlashSizeFixed {
                size: 0x10000,
                requested: 0x20000
            }]
        );
        assert_eq!(
            fixed.resolve_for(Some(0x20000)).unwrap_err().to_string(),
            "the layout has a fixed flash size of 0x10000 bytes and cannot be resolved for 0x20000: only a layout with size = \"detect\" can"
        );
        assert_eq!(fixed.resolve_for(None), fixed.resolve());

        let layout = layout_a();
        assert_eq!(
            layout.resolve_for(Some(0x200000)).unwrap_err().0,
            vec![LayoutError::FlashBelowMinimum {
                size: 0x200000,
                minimum: 0x400000
            }]
        );
        assert_eq!(
            layout.resolve_for(Some(0x200000)).unwrap_err().to_string(),
            "the flash of 0x200000 bytes is smaller than the minimum of 0x400000 bytes of the layout"
        );
        assert!(layout.resolve_for(Some(0x400800)).unwrap_err().0.contains(
            &LayoutError::FlashSizeNotAligned {
                size: 0x400800,
                alignment: 0x1000
            }
        ));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn the_detect_fixture_on_8_mib_is_the_8_mib_fixture() {
        let detect =
            Layout::from_toml(include_str!("../../tests/layouts/esp32-s3-detect.toml")).unwrap();
        let fixed =
            Layout::from_toml(include_str!("../../tests/layouts/esp32-s3-8mb.toml")).unwrap();
        let on_8_mib = detect.resolve_for(Some(8 << 20)).unwrap();
        let expected = fixed.resolve().unwrap();
        assert_eq!(spans(&on_8_mib), spans(&expected));
        assert_eq!(on_8_mib.flash.size, 8 << 20);
        let minimum = detect.resolve().unwrap();
        assert_eq!(at(&minimum, "storage"), (0x320000, 0xE0000));
        assert_eq!(
            flags(&minimum).last(),
            Some(&("storage", true, false)),
            "{:?}",
            flags(&minimum)
        );
        assert!(
            flags(&minimum)
                .iter()
                .rev()
                .skip(1)
                .all(|(_, grows, moves)| !grows && !moves)
        );
        assert_eq!(flags(&expected).last(), Some(&("storage", false, false)));
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

    fn arbitrary_role() -> impl Strategy<Value = Role> {
        prop_oneof![
            4 => Just(Role::Custom),
            1 => Just(Role::Bootloader),
            1 => Just(Role::PartitionTable),
            1 => Just(Role::Firmware),
            1 => (0u8..2).prop_map(|slot| Role::Update { slot }),
            1 => Just(Role::Storage),
        ]
    }

    /// Layouts of any shape, with growing sizes, a `detect` flash or not, and the flash size to
    /// resolve against, if any.
    fn arbitrary_layout() -> impl Strategy<Value = (Layout, Option<u64>)> {
        let alignment = (0u32..=16).prop_map(|shift| 1u64 << shift);
        let flash_size = prop_oneof![4 => 0u64..0x400000, 1 => any::<u64>()];
        let size = prop_oneof![
            4 => (1u64..0x40).prop_map(|units| Size::Bytes(units * 0x1000)),
            1 => any::<u64>().prop_map(Size::Bytes),
            1 => Just(Size::Remaining),
            2 => (1u64..0x40, 1u32..4, proptest::option::of(1u64..0x80)).prop_map(
                |(minimum, weight, maximum)| {
                    grow(minimum * 0x1000, weight, maximum.map(|maximum| maximum * 0x1000))
                }
            ),
            1 => (any::<u64>(), 1u32..=u32::MAX, proptest::option::of(any::<u64>()))
                .prop_map(|(minimum, weight, maximum)| grow(minimum, weight, maximum)),
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
        let requested = prop_oneof![
            2 => Just(None),
            1 => (0u64..0x80).prop_map(Some),
            1 => any::<u64>().prop_map(Some),
        ];
        (
            alignment,
            flash_size,
            any::<bool>(),
            any::<u64>(),
            requested,
            proptest::collection::vec((offset, size, partition_alignment, arbitrary_role()), 1..12),
        )
            .prop_map(
                |(alignment, size, detect, base_address, requested, parts)| {
                    let size = size - size % alignment;
                    // Mostly a size the layout may be resolved for, sometimes any.
                    let requested = requested.map(|more| {
                        if more < 0x80 {
                            size.saturating_add(more.saturating_mul(alignment))
                        } else {
                            more
                        }
                    });
                    let layout = Layout {
                        flash: Flash {
                            size,
                            alignment,
                            detect,
                            base_address,
                        },
                        partitions: parts
                            .into_iter()
                            .enumerate()
                            .map(|(index, (offset, size, alignment, role))| {
                                let mut partition = partition(&format!("p{index}"), offset, size);
                                partition.alignment = alignment;
                                partition.role = role;
                                partition
                            })
                            .collect(),
                    };
                    (layout, requested)
                },
            )
    }

    /// Layouts that must resolve: aligned sizes, ascending fixed offsets or none, no `remaining`,
    /// everything inside the flash.
    fn valid_layout() -> impl Strategy<Value = Layout> {
        let alignment = 0x1000u64;
        (
            0u64..0x10,
            proptest::collection::vec((any::<bool>(), 0u64..4, 1u64..0x10), 1..12),
        )
            .prop_map(move |(spare, parts)| {
                let mut cursor = 0u64;
                let mut partitions = Vec::new();
                for (index, (fixed, gap, units)) in parts.into_iter().enumerate() {
                    let size = units * alignment;
                    let offset = if fixed {
                        cursor += gap * alignment;
                        Some(cursor)
                    } else {
                        None
                    };
                    cursor += size;
                    partitions.push(partition(&format!("p{index}"), offset, Size::Bytes(size)));
                }
                Layout {
                    flash: flash(cursor + spare * alignment, alignment),
                    partitions,
                }
            })
    }

    proptest! {
        #[test]
        fn valid_layouts_resolve_soundly(layout in valid_layout()) {
            let resolved = layout.resolve();
            prop_assert!(resolved.is_ok(), "{:?}", resolved);
            let resolved = resolved.unwrap();
            prop_assert_eq!(resolved.partitions.len(), layout.partitions.len());
            let mut previous_end = 0u64;
            for partition in &resolved.partitions {
                prop_assert_eq!(partition.offset % partition.alignment, 0);
                prop_assert!(partition.offset >= previous_end);
                previous_end = partition.end();
            }
            prop_assert!(previous_end <= layout.flash.size);
            let used: u64 = resolved.partitions.iter().map(|partition| partition.size).sum();
            prop_assert_eq!(resolved.unallocated(), layout.flash.size - used);
        }
    }

    proptest! {
        #[test]
        fn resolution_never_panics_and_ok_means_sound((layout, requested) in arbitrary_layout()) {
            let result = layout.resolve_for(requested);
            prop_assert_eq!(&layout.resolve_for(requested), &result);
            if let Ok(resolved) = result {
                let flash_size = requested.unwrap_or(layout.flash.size);
                prop_assert_eq!(resolved.flash.size, flash_size);
                prop_assert_eq!(resolved.flash.detect, layout.flash.detect);
                prop_assert_eq!(
                    resolved.minimum_flash_size,
                    layout.flash.detect.then_some(layout.flash.size)
                );
                prop_assert_eq!(resolved.partitions.len(), layout.partitions.len());
                let mut spans = Vec::new();
                for (partition, source) in resolved.partitions.iter().zip(&layout.partitions) {
                    prop_assert_eq!(&partition.definition, source);
                    prop_assert!(partition.alignment.is_power_of_two());
                    prop_assert_eq!(partition.offset % partition.alignment, 0);
                    prop_assert!(partition.size > 0);
                    prop_assert!(partition.size >= source.size.minimum());
                    if let Size::Grow { maximum: Some(maximum), .. } = source.size {
                        prop_assert!(partition.size <= maximum);
                    }
                    if let Size::Bytes(bytes) = source.size {
                        prop_assert_eq!(partition.size, bytes);
                    }
                    prop_assert!(partition.end() <= flash_size);
                    if let Some(fixed) = source.offset {
                        prop_assert_eq!(partition.offset, fixed);
                        prop_assert!(!partition.moves_with_flash);
                    }
                    if !layout.flash.detect {
                        prop_assert!(!partition.grows_with_flash && !partition.moves_with_flash);
                    }
                    if partition.grows_with_flash {
                        prop_assert!(source.size.grows());
                        let refused = matches!(source.role, Role::Firmware | Role::Update { .. });
                        prop_assert!(!refused, "{:?}", source.role);
                    }
                    if partition.moves_with_flash {
                        let refused = matches!(
                            source.role,
                            Role::Bootloader | Role::PartitionTable | Role::Firmware | Role::Update { .. }
                        );
                        prop_assert!(!refused, "{:?}", source.role);
                    }
                    spans.push((partition.offset, partition.end()));
                }
                spans.sort();
                for pair in spans.windows(2) {
                    prop_assert!(pair[0].1 <= pair[1].0);
                }
                prop_assert!(resolved.unallocated() <= flash_size);
            }
        }
    }

    /// `detect` layouts whose segments start in ascending order, most of them valid: mixed
    /// alignments, fixed, `remaining` and growing sizes, and roles.
    fn detect_layout() -> impl Strategy<Value = Layout> {
        let part = (
            prop_oneof![3 => Just(None), 1 => (0u64..4).prop_map(Some)],
            prop_oneof![
                3 => (1u64..0x10).prop_map(|units| Size::Bytes(units * 0x1000)),
                1 => Just(Size::Remaining),
                3 => (1u64..0x10, 1u32..5, proptest::option::of(0u64..0x40)).prop_map(
                    |(minimum, weight, room)| grow(
                        minimum * 0x1000,
                        weight,
                        room.map(|room| (minimum + room) * 0x1000)
                    )
                ),
            ],
            (12u32..=16).prop_map(|shift| 1u64 << shift),
            arbitrary_role(),
        );
        (0u64..0x40, proptest::collection::vec(part, 1..10)).prop_map(|(spare, parts)| {
            let mut cursor = 0u64;
            let mut partitions = Vec::new();
            for (index, (gap, size, alignment, role)) in parts.into_iter().enumerate() {
                let offset = gap.map(|gap| align_up(cursor + gap * 0x1000, alignment).unwrap());
                let start = offset.unwrap_or_else(|| align_up(cursor, alignment).unwrap());
                cursor = start + size.minimum();
                let mut partition = partition(&format!("p{index}"), offset, size);
                partition.alignment = Some(alignment);
                partition.role = role;
                partitions.push(partition);
            }
            Layout {
                flash: Flash {
                    size: align_up(cursor, 0x1000).unwrap() + spare * 0x1000,
                    alignment: 0x1000,
                    detect: true,
                    base_address: 0,
                },
                partitions,
            }
        })
    }

    proptest! {
        #[test]
        fn a_larger_flash_never_breaks_or_shrinks(layout in detect_layout(), more in 0u64..64) {
            if let Ok(minimum) = layout.resolve() {
                let size = layout.flash.size + more * layout.flash.alignment;
                let larger = layout.resolve_for(Some(size));
                prop_assert!(larger.is_ok(), "{:?}", larger);
                let larger = larger.unwrap();
                let last_segment = layout
                    .partitions
                    .iter()
                    .rposition(|partition| partition.offset.is_some())
                    .unwrap_or(0);
                for (index, (small, large)) in
                    minimum.partitions.iter().zip(&larger.partitions).enumerate()
                {
                    prop_assert!(large.size >= small.size);
                    if let Some(fixed) = small.definition.offset {
                        prop_assert_eq!(large.offset, fixed);
                    }
                    if index < last_segment {
                        prop_assert_eq!(large, small);
                    } else if !small.grows_with_flash {
                        prop_assert_eq!(large.size, small.size);
                    }
                    if !small.moves_with_flash {
                        prop_assert_eq!(large.offset, small.offset);
                    }
                    prop_assert_eq!(large.grows_with_flash, small.grows_with_flash);
                    prop_assert_eq!(large.moves_with_flash, small.moves_with_flash);
                }
            }
        }
    }

    /// Layouts without growth or `detect`, of any shape.
    fn legacy_layout() -> impl Strategy<Value = Layout> {
        arbitrary_layout().prop_map(|(mut layout, _)| {
            layout.flash.detect = false;
            for partition in &mut layout.partitions {
                if let Size::Grow { minimum, .. } = partition.size {
                    partition.size = Size::Bytes(minimum);
                }
            }
            layout
        })
    }

    /// Layouts valid by the old rules: one `remaining` at most, at the end or before a fixed
    /// offset, after partitions of mixed alignments.
    fn valid_legacy_layout() -> impl Strategy<Value = Layout> {
        let part = (
            prop_oneof![3 => Just(None), 1 => (0u64..4).prop_map(Some)],
            1u64..0x20,
            prop_oneof![
                2 => Just(None),
                1 => (0u32..=16).prop_map(|shift| Some(1u64 << shift)),
            ],
            arbitrary_role(),
        );
        (
            proptest::collection::vec(part, 1..10),
            any::<prop::sample::Index>(),
            any::<bool>(),
            0u64..0x10,
            (10u32..=13).prop_map(|shift| 1u64 << shift),
        )
            .prop_map(|(parts, chosen, with_remaining, spare, flash_alignment)| {
                let count = parts.len();
                let remaining = with_remaining.then(|| chosen.index(count));
                let mut cursor = 0u64;
                let mut partitions: Vec<Partition> = Vec::new();
                let mut used_slots = Vec::new();
                for (index, (gap, units, alignment, mut role)) in parts.into_iter().enumerate() {
                    if let Role::Update { slot } = role {
                        if used_slots.contains(&slot) {
                            role = Role::Custom;
                        } else {
                            used_slots.push(slot);
                        }
                    }
                    let own = alignment.unwrap_or(flash_alignment);
                    let after_remaining = remaining.is_some_and(|chosen| chosen + 1 == index);
                    // The partition after a `remaining` needs a fixed offset.
                    let gap = if after_remaining {
                        Some(gap.unwrap_or(0))
                    } else {
                        gap
                    };
                    let size = units * 0x400;
                    let offset = gap.map(|gap| align_up(cursor + (gap + 1) * size, own).unwrap());
                    let start = offset.unwrap_or_else(|| align_up(cursor, own).unwrap());
                    let size = if remaining == Some(index) {
                        Size::Remaining
                    } else {
                        Size::Bytes(size)
                    };
                    cursor = start + size.minimum();
                    let mut partition = partition(&format!("p{index}"), offset, size);
                    partition.alignment = alignment;
                    partition.role = role;
                    partitions.push(partition);
                }
                let end = align_up(cursor + 1, flash_alignment).unwrap();
                Layout {
                    flash: flash(end + spare * flash_alignment, flash_alignment),
                    partitions,
                }
            })
    }

    fn resolves_as_before(layout: &Layout) -> Result<bool, TestCaseError> {
        let Some(before) = legacy::resolve(layout) else {
            return Ok(false);
        };
        let now = layout.resolve();
        prop_assert_eq!(now, Ok(before));
        Ok(true)
    }

    proptest! {
        #[test]
        fn layouts_valid_before_resolve_as_before(layout in legacy_layout()) {
            resolves_as_before(&layout)?;
        }

        #[test]
        fn valid_layouts_of_before_resolve_as_before(layout in valid_legacy_layout()) {
            prop_assert!(resolves_as_before(&layout)?, "{:?}", layout);
        }
    }
}
