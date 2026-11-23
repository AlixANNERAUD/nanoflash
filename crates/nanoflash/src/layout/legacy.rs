//! The resolver as it was before growing partitions, kept for tests only: every layout it
//! accepts must resolve to the same result today.
//!
//! It ignores `detect`, refuses `Grow`, and tracks its errors as a single flag.

use super::{Layout, Partition, ResolvedLayout, ResolvedPartition, Role, Size};
use alloc::vec::Vec;

/// The resolution of `layout` by the old rules, or `None` where they found an error.
pub(super) fn resolve(layout: &Layout) -> Option<ResolvedLayout> {
    let mut failed = false;
    let flash = &layout.flash;

    if layout.partitions.is_empty() || flash.size == 0 {
        failed = true;
    }
    if flash.alignment.is_power_of_two() {
        if flash.size % flash.alignment != 0 {
            failed = true;
        }
    } else {
        failed = true;
    }

    if !names_and_slots_are_valid(&layout.partitions) {
        failed = true;
    }

    let mut resolved: Vec<ResolvedPartition> = Vec::with_capacity(layout.partitions.len());
    let mut cursor = 0u64;
    for (index, partition) in layout.partitions.iter().enumerate() {
        let alignment = match partition.alignment {
            Some(own) if !own.is_power_of_two() => {
                failed = true;
                1
            }
            Some(own) => own,
            None if !flash.alignment.is_power_of_two() => 1,
            None => flash.alignment,
        };

        let offset = match partition.offset {
            Some(offset) => {
                if offset % alignment != 0 {
                    failed = true;
                }
                offset
            }
            None => match align_up(cursor, alignment) {
                Some(offset) => offset,
                None => {
                    failed = true;
                    continue;
                }
            },
        };

        let size = match partition.size {
            Size::Bytes(0) => {
                failed = true;
                continue;
            }
            Size::Bytes(bytes) => bytes,
            Size::Grow { .. } => {
                failed = true;
                continue;
            }
            Size::Remaining => {
                let next = layout.partitions.get(index + 1);
                let limit = match next {
                    Some(next) => match next.offset {
                        Some(limit) => limit,
                        None => {
                            failed = true;
                            continue;
                        }
                    },
                    None => flash.size,
                };
                match limit.checked_sub(offset) {
                    Some(size) if size > 0 => size,
                    _ => {
                        failed = true;
                        continue;
                    }
                }
            }
        };

        match offset.checked_add(size) {
            Some(end) => cursor = end,
            None => {
                failed = true;
                continue;
            }
        }
        resolved.push(ResolvedPartition {
            offset,
            size,
            alignment,
            grows_with_flash: false,
            moves_with_flash: false,
            definition: partition.clone(),
        });
    }

    let mut by_offset: Vec<&ResolvedPartition> = resolved.iter().collect();
    by_offset.sort_by_key(|partition| partition.offset);
    for (index, second) in by_offset.iter().enumerate() {
        for first in by_offset.iter().take(index) {
            if first.end() > second.offset {
                failed = true;
            }
        }
    }
    for partition in &resolved {
        if partition.end() > flash.size {
            failed = true;
        }
    }

    (!failed).then(|| ResolvedLayout {
        flash: flash.clone(),
        minimum_flash_size: None,
        partitions: resolved,
    })
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let mask = alignment.checked_sub(1)?;
    Some(value.checked_add(mask)? & !mask)
}

fn names_and_slots_are_valid(partitions: &[Partition]) -> bool {
    let mut valid = true;
    let mut remaining = 0;
    for (index, partition) in partitions.iter().enumerate() {
        let mut earlier = partitions.iter().take(index);
        if partition.name.is_empty() || earlier.any(|other| other.name == partition.name) {
            valid = false;
        }
        if let Role::Update { slot } = partition.role {
            if partitions
                .iter()
                .take(index)
                .any(|other| other.role == Role::Update { slot })
            {
                valid = false;
            }
        }
        if partition.size == Size::Remaining {
            remaining += 1;
        }
    }
    valid && remaining <= 1
}
