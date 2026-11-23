//! What grew and what moved when a layout is resolved for a larger flash.

use super::ResolvedLayout;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// A partition that is larger than on the minimum flash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrownPartition {
    /// The name of the partition.
    pub name: String,
    /// Its size on the minimum flash.
    pub size_at_minimum: u64,
    /// Its size.
    pub size: u64,
}

/// A partition that is at another offset than on the minimum flash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedPartition {
    /// The name of the partition.
    pub name: String,
    /// Its offset on the minimum flash.
    pub offset_at_minimum: u64,
    /// Its offset.
    pub offset: u64,
}

/// What differs from the layout resolved for the minimum flash.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutChanges {
    /// The partitions whose size differs, in file order.
    pub grown: Vec<GrownPartition>,
    /// The partitions whose offset differs, in file order.
    pub moved: Vec<MovedPartition>,
}

impl LayoutChanges {
    /// Whether nothing grew and nothing moved.
    pub fn is_empty(&self) -> bool {
        self.grown.is_empty() && self.moved.is_empty()
    }
}

impl ResolvedLayout {
    /// The partitions that grew and moved, compared with the same layout on `minimum`.
    ///
    /// Partitions are paired by name; one missing from `minimum` is left out.
    pub fn changes_from(&self, minimum: &ResolvedLayout) -> LayoutChanges {
        let mut changes = LayoutChanges::default();
        for partition in &self.partitions {
            let name = &partition.definition.name;
            let Some(base) = minimum.partition(name) else {
                continue;
            };
            if partition.size != base.size {
                changes.grown.push(GrownPartition {
                    name: name.clone(),
                    size_at_minimum: base.size,
                    size: partition.size,
                });
            }
            if partition.offset != base.offset {
                changes.moved.push(MovedPartition {
                    name: name.clone(),
                    offset_at_minimum: base.offset,
                    offset: partition.offset,
                });
            }
        }
        changes
    }
}
