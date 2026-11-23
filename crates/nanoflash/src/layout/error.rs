//! The errors found while checking a layout.

use super::Role;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use serde::Serialize;

/// One problem found in a layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LayoutError {
    /// The layout has no partition.
    EmptyLayout,
    /// The flash has a size of zero.
    ZeroFlashSize,
    /// An alignment is zero or not a power of two (`None` is the flash default).
    InvalidAlignment {
        /// The partition, or `None` for the flash default.
        partition: Option<String>,
        /// The offending alignment.
        alignment: u64,
    },
    /// The flash size is not a multiple of the default alignment.
    FlashSizeNotAligned {
        /// The flash size.
        size: u64,
        /// The default alignment.
        alignment: u64,
    },
    /// Two partitions share a name.
    DuplicateName {
        /// The name.
        name: String,
    },
    /// A partition has an empty name.
    EmptyName,
    /// A partition has a size of zero.
    ZeroSize {
        /// The partition.
        partition: String,
    },
    /// A fixed offset is not a multiple of the alignment.
    MisalignedOffset {
        /// The partition.
        partition: String,
        /// Its offset.
        offset: u64,
        /// Its alignment.
        alignment: u64,
    },
    /// Two partitions overlap.
    Overlap {
        /// The partition that comes first by offset.
        first: String,
        /// The partition that comes second by offset.
        second: String,
        /// The exclusive end of the first partition.
        first_end: u64,
        /// The offset of the second partition.
        second_offset: u64,
    },
    /// A partition ends after the end of the flash.
    ExceedsFlash {
        /// The partition.
        partition: String,
        /// Its exclusive end.
        end: u64,
        /// The flash size.
        flash_size: u64,
    },
    /// A partition taking the remaining space has no space left.
    NoSpaceForRemaining {
        /// The partition.
        partition: String,
        /// Where it starts.
        offset: u64,
        /// The address where the space ends.
        limit: u64,
        /// The partition that starts at `limit`, or `None` when the flash ends there.
        limited_by: Option<String>,
    },
    /// A partition taking the remaining space shares it with others, and its share rounds down
    /// to zero bytes.
    NoShareForRemaining {
        /// The partition.
        partition: String,
        /// Where the free space of its segment starts.
        start: u64,
        /// Where the free space of its segment ends.
        limit: u64,
        /// Its share is a multiple of this, the largest alignment of the partitions after it.
        step: u64,
        /// The partition after it whose alignment is `step`, or `None` for a step of one byte.
        set_by: Option<String>,
    },
    /// A fixed offset of a `detect` layout is below an earlier fixed offset.
    FixedOffsetBelowPrevious {
        /// The partition.
        partition: String,
        /// Its fixed offset.
        offset: u64,
        /// The partition with a fixed offset before it.
        previous: String,
        /// The fixed offset of `previous`.
        previous_offset: u64,
    },
    /// A partition whose role needs a fixed size grows.
    RoleCannotGrow {
        /// The partition.
        partition: String,
        /// Its role.
        role: Role,
    },
    /// A growing partition has a maximum below its minimum.
    InvalidGrowth {
        /// The partition.
        partition: String,
        /// Its minimum.
        minimum: u64,
        /// Its maximum.
        maximum: u64,
    },
    /// A partition whose role needs a known offset would move with the flash of the device.
    OffsetDependsOnFlash {
        /// The partition.
        partition: String,
        /// The first partition before it that grows with the flash.
        after: String,
    },
    /// A layout with a fixed flash size was asked to resolve for another size.
    FlashSizeFixed {
        /// The flash size of the layout.
        size: u64,
        /// The size asked for.
        requested: u64,
    },
    /// The flash is smaller than the minimum of a `detect` layout.
    FlashBelowMinimum {
        /// The size of the flash.
        size: u64,
        /// The minimum of the layout.
        minimum: u64,
    },
    /// Two update partitions use the same slot.
    DuplicateSlot {
        /// The slot number.
        slot: u8,
        /// The first partition.
        first: String,
        /// The second partition.
        second: String,
    },
    /// An offset or a size does not fit in 64 bits.
    Overflow {
        /// The partition.
        partition: String,
    },
    /// A partition name that is not in the layout.
    UnknownPartition {
        /// The name.
        name: String,
    },
    /// An image does not fit in its partition.
    ImageTooLarge {
        /// The partition.
        partition: String,
        /// The size of the image.
        image_size: u64,
        /// The size of the partition.
        partition_size: u64,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLayout => formatter.write_str("the layout has no partition"),
            Self::ZeroFlashSize => formatter.write_str("the flash size is zero"),
            Self::InvalidAlignment {
                partition: Some(partition),
                alignment,
            } => write!(
                formatter,
                "the alignment {alignment:#x} of partition \"{partition}\" is not a power of two"
            ),
            Self::InvalidAlignment {
                partition: None,
                alignment,
            } => write!(
                formatter,
                "the default flash alignment {alignment:#x} is not a power of two"
            ),
            Self::FlashSizeNotAligned { size, alignment } => write!(
                formatter,
                "the flash size {size:#x} is not a multiple of the alignment {alignment:#x}"
            ),
            Self::DuplicateName { name } => {
                write!(formatter, "several partitions are named \"{name}\"")
            }
            Self::EmptyName => formatter.write_str("a partition has an empty name"),
            Self::ZeroSize { partition } => {
                write!(formatter, "partition \"{partition}\" has a size of zero")
            }
            Self::MisalignedOffset {
                partition,
                offset,
                alignment,
            } => write!(
                formatter,
                "partition \"{partition}\" starts at {offset:#x}, which is not a multiple of its alignment {alignment:#x}"
            ),
            Self::Overlap {
                first,
                second,
                first_end,
                second_offset,
            } => write!(
                formatter,
                "partitions \"{first}\" and \"{second}\" overlap: \"{first}\" ends at {first_end:#x} but \"{second}\" starts at {second_offset:#x}"
            ),
            Self::ExceedsFlash {
                partition,
                end,
                flash_size,
            } => write!(
                formatter,
                "partition \"{partition}\" ends at {end:#x}, past the end of the flash at {flash_size:#x}"
            ),
            Self::NoSpaceForRemaining {
                partition,
                offset,
                limit,
                limited_by: Some(next),
            } => write!(
                formatter,
                "partition \"{partition}\" uses the remaining space but starts at {offset:#x}, and the next partition \"{next}\" starts at {limit:#x}, so no space is left"
            ),
            Self::NoSpaceForRemaining {
                partition,
                offset,
                limit,
                limited_by: None,
            } => write!(
                formatter,
                "partition \"{partition}\" uses the remaining space but starts at {offset:#x}, and the flash ends at {limit:#x}, so no space is left"
            ),
            Self::NoShareForRemaining {
                partition,
                start,
                limit,
                step,
                set_by,
            } => {
                write!(
                    formatter,
                    "partition \"{partition}\" uses the remaining space, but its share of the free space from {start:#x} to {limit:#x} rounds down to zero bytes in steps of {step:#x}"
                )?;
                match set_by {
                    Some(set_by) => write!(
                        formatter,
                        ", the alignment of partition \"{set_by}\" after it"
                    ),
                    None => Ok(()),
                }
            }
            Self::FixedOffsetBelowPrevious {
                partition,
                offset,
                previous,
                previous_offset,
            } => write!(
                formatter,
                "partition \"{partition}\" has a fixed offset of {offset:#x}, below {previous_offset:#x} of \"{previous}\" before it: in a layout with size = \"detect\", fixed offsets ascend in file order"
            ),
            Self::RoleCannotGrow { partition, role } => write!(
                formatter,
                "partition \"{partition}\" has role {} and cannot grow: give it a fixed size",
                role_name(*role)
            ),
            Self::InvalidGrowth {
                partition,
                minimum,
                maximum,
            } => write!(
                formatter,
                "partition \"{partition}\" may grow to a maximum of {maximum:#x} bytes, less than its minimum of {minimum:#x} bytes"
            ),
            Self::OffsetDependsOnFlash { partition, after } => write!(
                formatter,
                "partition \"{partition}\" comes after \"{after}\", which grows with the flash of the device, so its offset would depend on the device: give \"{partition}\" a fixed offset or put it before \"{after}\""
            ),
            Self::FlashSizeFixed { size, requested } => write!(
                formatter,
                "the layout has a fixed flash size of {size:#x} bytes and cannot be resolved for {requested:#x}: only a layout with size = \"detect\" can"
            ),
            Self::FlashBelowMinimum { size, minimum } => write!(
                formatter,
                "the flash of {size:#x} bytes is smaller than the minimum of {minimum:#x} bytes of the layout"
            ),
            Self::DuplicateSlot {
                slot,
                first,
                second,
            } => write!(
                formatter,
                "partitions \"{first}\" and \"{second}\" both use update slot {slot}"
            ),
            Self::Overflow { partition } => write!(
                formatter,
                "the offset or the size of partition \"{partition}\" does not fit in 64 bits"
            ),
            Self::UnknownPartition { name } => {
                write!(formatter, "there is no partition named \"{name}\"")
            }
            Self::ImageTooLarge {
                partition,
                image_size,
                partition_size,
            } => write!(
                formatter,
                "the image of {image_size:#x} bytes does not fit in partition \"{partition}\" of {partition_size:#x} bytes"
            ),
        }
    }
}

impl core::error::Error for LayoutError {}

/// The role as a layout file writes it.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Bootloader => "bootloader",
        Role::PartitionTable => "partition-table",
        Role::Firmware => "firmware",
        Role::Update { .. } => "update",
        Role::Storage => "storage",
        Role::CrashRecord => "crash-record",
        Role::Custom => "custom",
    }
}

/// Every problem found in a layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayoutErrors(pub Vec<LayoutError>);

impl fmt::Display for LayoutErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            write!(formatter, "{error}")?;
        }
        Ok(())
    }
}

impl core::error::Error for LayoutErrors {}
