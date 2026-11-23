//! Writers of the partition table each family needs.

#[cfg(feature = "table-espressif")]
pub mod espressif;

use alloc::string::String;
use core::fmt;
use serde::Serialize;

/// A layout that a partition table writer cannot express.
///
/// Shared by every writer, so a variant may only be produced by some of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TableError {
    /// An attribute the writer needs is neither given nor implied by the role.
    MissingAttribute {
        /// The partition.
        partition: String,
        /// The writer, such as `espressif`.
        writer: &'static str,
        /// The attribute, such as `subtype`.
        attribute: &'static str,
    },
    /// An attribute holds a name or number the writer does not know.
    UnknownValue {
        /// The partition.
        partition: String,
        /// The attribute, such as `subtype`.
        attribute: &'static str,
        /// The value as given.
        value: String,
    },
    /// A partition name is longer than the table allows.
    LabelTooLong {
        /// The partition.
        partition: String,
    },
    /// A partition name holds characters outside ASCII.
    NotAscii {
        /// The partition.
        partition: String,
    },
    /// An offset is not a multiple of what the table requires.
    Misaligned {
        /// The partition.
        partition: String,
        /// Its offset.
        offset: u64,
        /// The required alignment.
        required: u64,
    },
    /// An offset, size or number does not fit the field of the table.
    OutOfRange {
        /// The partition.
        partition: String,
    },
    /// The layout has more partitions than the table holds.
    TooManyPartitions {
        /// The number of partitions to write.
        count: usize,
        /// The number the table holds.
        maximum: usize,
    },
    /// A partition the table depends on is missing.
    MissingPartition {
        /// What is missing.
        role: &'static str,
    },
    /// The partition meant to hold the table is too small for it.
    TableDoesNotFit {
        /// The partition.
        partition: String,
        /// Its size.
        size: u64,
        /// The size the table needs.
        required: u64,
    },
    /// Two partitions both claim to be the firmware.
    SeveralFirmware {
        /// The first partition.
        first: String,
        /// The second partition.
        second: String,
    },
}

impl fmt::Display for TableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableError::MissingAttribute {
                partition,
                writer,
                attribute,
            } => write!(
                formatter,
                "the {writer} partition table cannot express partition \"{partition}\": it needs the \"{attribute}\" attribute, set under [partition.{writer}]"
            ),
            TableError::UnknownValue {
                partition,
                attribute,
                value,
            } => write!(
                formatter,
                "the espressif partition table cannot express partition \"{partition}\": \"{value}\" is not a known {attribute}"
            ),
            TableError::LabelTooLong { partition } => write!(
                formatter,
                "the espressif partition table cannot express partition \"{partition}\": its name is longer than 16 bytes"
            ),
            TableError::NotAscii { partition } => write!(
                formatter,
                "the espressif partition table cannot express partition \"{partition}\": its name is not ASCII"
            ),
            TableError::Misaligned {
                partition,
                offset,
                required,
            } => write!(
                formatter,
                "the espressif partition table cannot express partition \"{partition}\": its offset {offset:#x} is not a multiple of {required:#x}"
            ),
            TableError::OutOfRange { partition } => write!(
                formatter,
                "the espressif partition table cannot express partition \"{partition}\": its offset, size, slot or number is out of range"
            ),
            TableError::TooManyPartitions { count, maximum } => write!(
                formatter,
                "the espressif partition table holds at most {maximum} partitions but the layout has {count}"
            ),
            TableError::MissingPartition { role } => write!(
                formatter,
                "the espressif partition table needs a partition for {role}, and the layout has none"
            ),
            TableError::TableDoesNotFit {
                partition,
                size,
                required,
            } => write!(
                formatter,
                "the espressif partition table does not fit in partition \"{partition}\": it is {size:#x} bytes and the table needs {required:#x}"
            ),
            TableError::SeveralFirmware { first, second } => write!(
                formatter,
                "the espressif partition table cannot express partitions \"{first}\" and \"{second}\": both are the factory application"
            ),
        }
    }
}

impl core::error::Error for TableError {}
