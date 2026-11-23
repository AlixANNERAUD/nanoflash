//! The errors found while checking a layout.

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
    /// Several partitions take the remaining space.
    SeveralRemaining {
        /// The first one in file order.
        first: String,
        /// The second one in file order.
        second: String,
    },
    /// A partition taking the remaining space has no space, or is followed by a partition without
    /// a fixed offset.
    NoSpaceForRemaining {
        /// The partition.
        partition: String,
        /// Where it starts.
        offset: u64,
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
            Self::SeveralRemaining { first, second } => write!(
                formatter,
                "partitions \"{first}\" and \"{second}\" both take the remaining space"
            ),
            Self::NoSpaceForRemaining { partition, offset } => write!(
                formatter,
                "partition \"{partition}\" takes the remaining space but none is left after {offset:#x}, or the next partition has no fixed offset"
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
