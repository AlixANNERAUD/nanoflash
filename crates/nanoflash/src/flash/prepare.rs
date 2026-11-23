//! Reading the images of a board, placing them in flash and checking the plan.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::intel_hex::{IntelHexError, decode_intel_hex, is_eeprom_image, is_intel_hex};
use super::{ImageSource, ImageVerifier};
use crate::backend::Region;
use crate::error::Error;
use crate::layout::{GrownPartition, Layout, LayoutChanges, MovedPartition, ResolvedLayout, Role};
use crate::manifest::{Board, EraseTarget, ImageTarget, Manifest, Protocol};
use crate::table::espressif::{TABLE_SIZE, write};

/// The erase unit of a protocol: images start at a multiple of it, and the sectors an image ends
/// in are erased whole. The Espressif ROM loader erases sectors of 4 KiB; the nRF52840 programs
/// and erases whole 4 KiB pages, which the 256-byte rows of the SAMD21 divide. The AVR
/// bootloaders write whole pages: the largest page of the chips of each protocol.
pub(crate) fn erase_unit(protocol: Protocol) -> u64 {
    match protocol {
        Protocol::Espressif | Protocol::Samba => 0x1000,
        Protocol::Stk500v1 | Protocol::Avr109 => 0x80,
        Protocol::Stk500v2 => 0x100,
    }
}

/// The end of what a protocol can address: its offsets and sizes are 32-bit words, except for
/// STK500v1 and AVR109, whose 16-bit word addresses reach 128 KiB.
pub(crate) fn address_limit(protocol: Protocol) -> u64 {
    match protocol {
        Protocol::Espressif | Protocol::Samba | Protocol::Stk500v2 => 1 << 32,
        Protocol::Stk500v1 | Protocol::Avr109 => 1 << 17,
    }
}

/// An image ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    /// The name for messages: the partition, or `image at 0x<offset>`.
    pub label: String,
    /// The layout partition that starts at the offset of the image, if any.
    pub partition: Option<String>,
    /// The flash offset of its first byte.
    pub offset: u64,
    /// Its bytes.
    pub bytes: Vec<u8>,
    /// How the offset was chosen, which says whether it follows the flash of the chip.
    pub placement: ImagePlacement,
}

/// How the offset of an image was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePlacement {
    /// The manifest names its partition: it goes where the partition is on the chip.
    Partition,
    /// The manifest gives its offset: it stays there.
    Offset,
    /// The partition table generated from the layout: generated again for the chip.
    GeneratedTable,
}

/// Something the user may want to know, which does not stop the flash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Warning {
    /// An image of the manifest replaces the partition table nanoflash generates from the layout,
    /// and differs from it.
    PartitionTableDiffers {
        /// The partition of the table.
        partition: String,
    },
    /// An image of the manifest replaces the partition table, and the table could not be generated
    /// from the layout to compare with.
    PartitionTableNotChecked {
        /// The partition of the table.
        partition: String,
        /// Why the table cannot be generated.
        message: String,
    },
    /// The chip is in secure download mode, whose ROM loader refuses `SPI_FLASH_MD5`: what was
    /// written was not read back.
    NotVerifiedInSecureDownloadMode,
    /// The bootloader erased from the lowest image to the end of flash before writing it (the
    /// SAMD21's `X`), unasked: what was stored there besides the images is gone.
    ErasesToEnd {
        /// The flash offset of the lowest image, where the erase started.
        from: u64,
        /// The end of flash, where it stopped.
        to: u64,
        /// The partitions of the layout in that range that no image is written to.
        partitions: Vec<String>,
    },
    /// The chip has more flash than the minimum of a `detect` layout: the partitions that grow
    /// with the flash are larger than on the minimum.
    PartitionsGrew {
        /// The flash size the chip reported.
        flash_size: u64,
        /// The minimum flash size of the layout.
        minimum_flash_size: u64,
        /// The partitions that grew, in file order.
        partitions: Vec<GrownPartition>,
        /// The protocol of the board: the Espressif bootloader reads a flash size of its own.
        protocol: Protocol,
    },
    /// The chip has more flash than the minimum of a `detect` layout: the partitions that move
    /// with the flash start elsewhere than on the minimum.
    PartitionsMoved {
        /// The flash size the chip reported.
        flash_size: u64,
        /// The minimum flash size of the layout.
        minimum_flash_size: u64,
        /// The partitions that moved, in file order.
        partitions: Vec<MovedPartition>,
    },
    /// The chip did not report its flash size: a `detect` layout stays resolved for its minimum.
    FlashSizeNotDetected {
        /// The minimum flash size of the layout.
        minimum_flash_size: u64,
    },
}

/// Writes each item, after `first` for the first and after `, ` for the others.
fn write_list<T>(
    formatter: &mut fmt::Formatter<'_>,
    items: &[T],
    first: &str,
    mut item: impl FnMut(&mut fmt::Formatter<'_>, &T) -> fmt::Result,
) -> fmt::Result {
    for (index, value) in items.iter().enumerate() {
        formatter.write_str(if index == 0 { first } else { ", " })?;
        item(formatter, value)?;
    }
    Ok(())
}

impl fmt::Display for Warning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::PartitionTableDiffers { partition } => write!(
                formatter,
                "the image for the partition \"{partition}\" differs from the partition table generated from the layout: the board will boot with the table of the image"
            ),
            Warning::PartitionTableNotChecked { partition, message } => write!(
                formatter,
                "the image for the partition \"{partition}\" was not compared with the partition table of the layout, which cannot be generated: {message}"
            ),
            Warning::NotVerifiedInSecureDownloadMode => formatter.write_str(
                "the chip is in secure download mode, whose ROM loader refuses to compute MD5 digests of the flash: what was written was not verified",
            ),
            Warning::ErasesToEnd {
                from,
                to,
                partitions,
            } => {
                write!(
                    formatter,
                    "the bootloader erased the flash from 0x{from:x} to its end at 0x{to:x} before writing, so what was stored there besides the images is gone"
                )?;
                write_list(
                    formatter,
                    partitions,
                    ", including the partitions ",
                    |formatter, partition| write!(formatter, "\"{partition}\""),
                )
            }
            Warning::PartitionsGrew {
                flash_size,
                minimum_flash_size,
                partitions,
                protocol,
            } => {
                write!(
                    formatter,
                    "the chip has 0x{flash_size:x} bytes of flash, more than the minimum of 0x{minimum_flash_size:x} of the layout, so partitions grew"
                )?;
                write_list(formatter, partitions, ": ", |formatter, partition| {
                    write!(
                        formatter,
                        "\"{}\" from 0x{:x} to 0x{:x} bytes",
                        partition.name, partition.size_at_minimum, partition.size
                    )
                })?;
                if *protocol == Protocol::Espressif {
                    formatter.write_str(
                        "; the bootloader image must be built for this flash size or with flash size detection",
                    )?;
                }
                Ok(())
            }
            Warning::PartitionsMoved {
                flash_size,
                minimum_flash_size,
                partitions,
            } => {
                write!(
                    formatter,
                    "on the 0x{flash_size:x} bytes of flash of the chip, partitions start elsewhere than on the minimum of 0x{minimum_flash_size:x}"
                )?;
                write_list(formatter, partitions, ": ", |formatter, partition| {
                    write!(
                        formatter,
                        "\"{}\" at 0x{:x} instead of 0x{:x}",
                        partition.name, partition.offset, partition.offset_at_minimum
                    )
                })?;
                formatter.write_str(
                    "; data stored there by a firmware for another flash size is not where the layout now puts it",
                )
            }
            Warning::FlashSizeNotDetected { minimum_flash_size } => write!(
                formatter,
                "the chip did not report its flash size, so the layout was resolved for its minimum of 0x{minimum_flash_size:x} bytes: the partitions that grow with the flash have their smallest size"
            ),
        }
    }
}

/// A region of flash to erase, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedErase {
    /// The name for messages: the partition, or `region at 0x<offset>`.
    pub label: String,
    /// The partition the manifest names, which the region follows on the chip; `None` for a
    /// region given by offset.
    pub partition: Option<String>,
    /// The region.
    pub region: Region,
}

/// What a board flashes, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The resolved layout of the board, when it has one: for its minimum flash size, or for
    /// the flash of the chip once resolved again.
    pub layout: Option<ResolvedLayout>,
    /// The layout as written, to resolve again for the flash of the chip.
    pub definition: Option<Layout>,
    /// The images, sorted as the manifest lists them, the generated table by its offset.
    pub images: Vec<PreparedImage>,
    /// The regions to erase first.
    pub erase: Vec<PreparedErase>,
    /// What the user may want to know.
    pub warnings: Vec<Warning>,
}

/// Where an image comes from.
enum Origin<'a> {
    /// A file of the manifest: its path and expected digest.
    File(&'a str, Option<[u8; 32]>),
    /// The table generated from the layout.
    Generated(Vec<u8>),
}

struct Placement<'a> {
    label: String,
    partition: Option<String>,
    placement: ImagePlacement,
    offset: u64,
    /// The size of the partition the image is meant for.
    partition_size: Option<u64>,
    origin: Origin<'a>,
}

/// The span of the largest flash of the chips nanoflash supports (32 MiB), the bound of an Intel
/// HEX image when the board has no layout.
const MAXIMUM_HEX_SPAN: u64 = 0x200_0000;

/// Decodes the Intel HEX `text` read from `path`.
fn intel_hex_image(
    path: &str,
    text: &[u8],
    maximum_span: u64,
) -> Result<super::IntelHexImage, Error> {
    decode_intel_hex(text, maximum_span).map_err(|error| match error {
        IntelHexError::Invalid { line, message } => Error::IntelHex {
            path: path.to_string(),
            line,
            message,
        },
        IntelHexError::OutOfMemory { size } => Error::OutOfMemory {
            what: format!("Intel HEX image \"{path}\""),
            size,
        },
    })
}

fn hexadecimal(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// The layout of the board as written, and resolved for its flash size (the minimum of a
/// `detect` layout).
async fn read_layout<S: ImageSource>(
    board: &Board,
    source: &mut S,
) -> Result<Option<(Layout, ResolvedLayout)>, Error> {
    let Some(path) = &board.layout else {
        return Ok(None);
    };
    let file_error = |message: String| Error::LayoutFile {
        path: path.clone(),
        message,
    };
    let bytes = source.read(path).await.map_err(file_error)?;
    let text = core::str::from_utf8(&bytes)
        .map_err(|error| file_error(format!("the file is not UTF-8 text: {error}")))?;
    let layout = Layout::from_toml(text).map_err(|error| file_error(error.message))?;
    let resolved = resolve(&layout, None)?;
    Ok(Some((layout, resolved)))
}

/// Resolves `layout` for `flash_size`, or for its own flash size.
fn resolve(layout: &Layout, flash_size: Option<u64>) -> Result<ResolvedLayout, Error> {
    layout
        .resolve_for(flash_size)
        .map_err(|errors| Error::Layout { errors: errors.0 })
}

/// The layout, or the error of a manifest entry that needs it.
fn need_layout<'l>(
    layout: &'l Option<ResolvedLayout>,
    board: &Board,
    partition: &str,
) -> Result<&'l ResolvedLayout, Error> {
    layout.as_ref().ok_or_else(|| Error::ImageNeedsLayout {
        board: board.identifier.clone(),
        partition: partition.to_string(),
    })
}

fn find_partition<'l>(
    layout: &'l ResolvedLayout,
    board: &Board,
    name: &str,
) -> Result<&'l crate::layout::ResolvedPartition, Error> {
    layout
        .partition(name)
        .ok_or_else(|| Error::UnknownPartition {
            board: board.identifier.clone(),
            partition: name.to_string(),
        })
}

fn round_up(value: u64, unit: u64) -> u64 {
    match value.checked_add(unit - 1) {
        Some(value) => value / unit * unit,
        None => u64::MAX,
    }
}

/// The first problem of the plan, checking the images in order, then their neighbours.
fn check_plan(
    protocol: Protocol,
    layout: &Option<ResolvedLayout>,
    images: &[PreparedImage],
    sizes: &[Option<u64>],
) -> Result<(), Error> {
    let unit = erase_unit(protocol);
    let limit = address_limit(protocol);
    for (image, partition_size) in images.iter().zip(sizes) {
        let size = u64::try_from(image.bytes.len()).unwrap_or(u64::MAX);
        if image.offset % unit != 0 {
            return Err(Error::MisalignedImage {
                label: image.label.clone(),
                offset: image.offset,
                required: unit,
            });
        }
        let end = image.offset.saturating_add(size);
        if end > limit {
            return Err(Error::OutsideAddressRange {
                what: format!("image \"{}\"", image.label),
                end,
                limit,
                protocol,
            });
        }
        if let (Some(partition), Some(partition_size)) = (&image.partition, partition_size) {
            if size > *partition_size {
                return Err(Error::ImageTooLarge {
                    label: image.label.clone(),
                    partition: partition.clone(),
                    image_size: size,
                    partition_size: *partition_size,
                });
            }
        }
        if let Some(layout) = layout {
            if end > layout.flash.size {
                return Err(Error::ImageOutsideFlash {
                    label: image.label.clone(),
                    end,
                    flash_size: layout.flash.size,
                });
            }
        }
    }
    let mut order: Vec<&PreparedImage> = images.iter().collect();
    order.sort_by_key(|image| image.offset);
    for pair in order.windows(2) {
        let [first, second] = pair else { continue };
        let size = u64::try_from(first.bytes.len()).unwrap_or(u64::MAX);
        let first_end = round_up(first.offset.saturating_add(size), unit);
        if first_end > second.offset {
            return Err(Error::OverlappingImages {
                first: first.label.clone(),
                second: second.label.clone(),
                first_end,
                second_offset: second.offset,
            });
        }
    }
    Ok(())
}

/// An input tied to flash offsets: an image or an erase entry given by offset, or an Intel HEX
/// image, whose addresses are in its file.
struct FixedSpan {
    what: String,
    start: u64,
    end: u64,
    intel_hex: bool,
}

/// The inputs of `board` tied to flash offsets, among the prepared `images` and `erase` entries.
///
/// An Intel HEX image is found by its target in the manifest: no two images share an offset, nor
/// a partition, once `check_plan` accepted them.
fn fixed_spans(board: &Board, images: &[PreparedImage], erase: &[PreparedErase]) -> Vec<FixedSpan> {
    let mut spans = Vec::new();
    for image in images {
        let target = |target: &ImageTarget| match (image.placement, target) {
            (ImagePlacement::Partition, ImageTarget::Partition(name)) => {
                image.partition.as_deref() == Some(name.as_str())
            }
            (ImagePlacement::Offset, ImageTarget::Offset(offset)) => image.offset == *offset,
            _ => false,
        };
        let hex = board
            .images
            .iter()
            .find(|manifest| target(&manifest.target) && is_intel_hex(&manifest.path));
        let what = match (hex, image.placement) {
            (Some(manifest), _) => format!("Intel HEX image \"{}\"", manifest.path),
            (None, ImagePlacement::Offset) => format!("image \"{}\"", image.label),
            (None, _) => continue,
        };
        let size = u64::try_from(image.bytes.len()).unwrap_or(u64::MAX);
        spans.push(FixedSpan {
            what,
            start: image.offset,
            end: image.offset.saturating_add(size),
            intel_hex: hex.is_some(),
        });
    }
    for entry in erase.iter().filter(|entry| entry.partition.is_none()) {
        spans.push(FixedSpan {
            what: format!("erase entry \"{}\"", entry.label),
            start: entry.region.offset,
            end: entry.region.offset.saturating_add(entry.region.size),
            intel_hex: false,
        });
    }
    spans
}

/// Refuses the first input tied to flash offsets that overlaps a partition of `layout` moving
/// with the flash: on another chip, the partition would be elsewhere and the input would not.
fn check_fixed_spans(layout: &ResolvedLayout, spans: &[FixedSpan]) -> Result<(), Error> {
    for span in spans {
        let moving = layout.partitions.iter().find(|partition| {
            partition.moves_with_flash
                && span.start < partition.end()
                && partition.offset < span.end
        });
        if let Some(partition) = moving {
            return Err(Error::FixedOffsetOnMovingPartition {
                what: span.what.clone(),
                start: span.start,
                end: span.end,
                partition: partition.definition.name.clone(),
                partition_offset: partition.offset,
                flash_size: layout.flash.size,
                intel_hex: span.intel_hex,
            });
        }
    }
    Ok(())
}

/// Checks that an erase entry starts and ends on the erase unit, inside what the protocol can
/// address and inside the flash of the layout.
fn check_erase(
    protocol: Protocol,
    layout: &Option<ResolvedLayout>,
    erase: &PreparedErase,
) -> Result<(), Error> {
    let unit = erase_unit(protocol);
    let limit = address_limit(protocol);
    let Region { offset, size } = erase.region;
    if offset % unit != 0 || size % unit != 0 {
        return Err(Error::MisalignedErase {
            entry: erase.label.clone(),
            offset,
            size,
            required: unit,
        });
    }
    let end = offset.saturating_add(size);
    if end > limit {
        return Err(Error::OutsideAddressRange {
            what: format!("erase entry \"{}\"", erase.label),
            end,
            limit,
            protocol,
        });
    }
    if let Some(layout) = layout {
        if end > layout.flash.size {
            return Err(Error::EraseOutsideFlash {
                entry: erase.label.clone(),
                end,
                flash_size: layout.flash.size,
            });
        }
    }
    Ok(())
}

/// Reads the layout and the images of `board`, and checks where they go.
///
/// Every image is read through `source`, checked against its digest and handed to `verifier`.
/// When the board speaks the Espressif protocol and its layout has a partition table, no image
/// lands on, the table generated from the layout is added. Images and erase entries are checked
/// against the erase unit and the address range of the protocol, and the flash of the layout.
/// A board without images and erase entries, and an empty image, are refused.
///
/// A file named `.hex` or `.ihex` is Intel HEX: the digest and the verifier see the text, the
/// image is the decoded span, and its lowest address must be the placement of the image plus the
/// base address of the flash of the layout. A file named `.eep` is refused: EEPROM is not written.
pub async fn prepare<S: ImageSource>(
    board: &Board,
    source: &mut S,
    verifier: &mut dyn ImageVerifier,
) -> Result<Prepared, Error> {
    if board.images.is_empty() && board.erase.is_empty() {
        return Err(Error::NothingToFlash {
            board: board.identifier.clone(),
        });
    }
    let (definition, layout) = match read_layout(board, source).await? {
        Some((definition, layout)) => (Some(definition), Some(layout)),
        None => (None, None),
    };

    let mut placements: Vec<Placement<'_>> = Vec::new();
    for image in &board.images {
        let origin = Origin::File(&image.path, image.sha256);
        placements.push(match &image.target {
            ImageTarget::Partition(name) => {
                let partition = find_partition(need_layout(&layout, board, name)?, board, name)?;
                Placement {
                    label: name.clone(),
                    partition: Some(name.clone()),
                    placement: ImagePlacement::Partition,
                    offset: partition.offset,
                    partition_size: Some(partition.size),
                    origin,
                }
            }
            ImageTarget::Offset(offset) => {
                let partition = layout.as_ref().and_then(|layout| {
                    layout
                        .partitions
                        .iter()
                        .find(|partition| partition.offset == *offset)
                });
                Placement {
                    label: format!("image at 0x{offset:x}"),
                    partition: partition.map(|partition| partition.definition.name.clone()),
                    placement: ImagePlacement::Offset,
                    offset: *offset,
                    partition_size: partition.map(|partition| partition.size),
                    origin,
                }
            }
        });
    }

    let mut warnings = Vec::new();
    // The image that replaces the generated table: its index, the name of the table partition and
    // the generated table, when it could be generated.
    let mut table_image: Option<(usize, String, Option<Vec<u8>>)> = None;
    let table_partition = match (&layout, board.protocol) {
        (Some(layout), Protocol::Espressif) => layout
            .partitions
            .iter()
            .find(|partition| partition.definition.role == Role::PartitionTable)
            .map(|table| (layout, table)),
        _ => None,
    };
    if let Some((layout, table)) = table_partition {
        let name = table.definition.name.clone();
        let generated = write(layout);
        match placements
            .iter()
            .position(|placement| placement.offset == table.offset)
        {
            Some(index) => match generated {
                Ok(generated) => table_image = Some((index, name, Some(generated))),
                Err(error) => {
                    warnings.push(Warning::PartitionTableNotChecked {
                        partition: name.clone(),
                        message: error.to_string(),
                    });
                    table_image = Some((index, name, None));
                }
            },
            None => {
                let generated = generated.map_err(|error| Error::Table { error })?;
                let position = placements
                    .iter()
                    .position(|placement| placement.offset > table.offset)
                    .unwrap_or(placements.len());
                placements.insert(
                    position,
                    Placement {
                        label: name.clone(),
                        partition: Some(name),
                        placement: ImagePlacement::GeneratedTable,
                        offset: table.offset,
                        partition_size: Some(table.size),
                        origin: Origin::Generated(generated),
                    },
                );
            }
        }
    }

    let mut images = Vec::with_capacity(placements.len());
    let mut sizes = Vec::with_capacity(placements.len());
    for (index, placement) in placements.into_iter().enumerate() {
        let bytes = match placement.origin {
            Origin::Generated(bytes) => bytes,
            Origin::File(path, expected) => {
                let bytes = source
                    .read(path)
                    .await
                    .map_err(|message| Error::ImageSource {
                        path: path.to_string(),
                        message,
                    })?;
                if bytes.is_empty() {
                    return Err(Error::EmptyImage {
                        path: path.to_string(),
                    });
                }
                if let Some(expected) = expected {
                    let actual = Sha256::digest(&bytes);
                    if actual.as_slice() != expected {
                        return Err(Error::ChecksumMismatch {
                            path: path.to_string(),
                            expected: hexadecimal(&expected),
                            actual: hexadecimal(&actual),
                        });
                    }
                }
                verifier
                    .verify(path, &bytes)
                    .map_err(|reason| Error::ImageRejected {
                        path: path.to_string(),
                        reason,
                    })?;
                if is_eeprom_image(path) {
                    return Err(Error::ImageRejected {
                        path: path.to_string(),
                        reason: "EEPROM images are not written; remove it from the manifest"
                            .to_string(),
                    });
                }
                if is_intel_hex(path) {
                    // Addresses of an Intel HEX file are the addresses of the memory map: the
                    // offset of the placement plus the base address of the flash of the layout.
                    let base = layout
                        .as_ref()
                        .map_or(0, |layout| layout.flash.base_address);
                    let expected = base.saturating_add(placement.offset);
                    let maximum = layout
                        .as_ref()
                        .map_or(MAXIMUM_HEX_SPAN, |layout| layout.flash.size);
                    let image = intel_hex_image(path, &bytes, maximum)?;
                    if image.start != expected {
                        return Err(Error::MisplacedIntelHex {
                            path: path.to_string(),
                            start: image.start,
                            expected,
                            base_address: base,
                            partition: placement.partition.clone(),
                        });
                    }
                    image.bytes
                } else {
                    bytes
                }
            }
        };
        if let Some((table_index, name, Some(generated))) = &table_image {
            if *table_index == index {
                let (head, rest) = bytes.split_at(bytes.len().min(TABLE_SIZE));
                if head != generated.as_slice() || rest.iter().any(|byte| *byte != 0xFF) {
                    warnings.push(Warning::PartitionTableDiffers {
                        partition: name.clone(),
                    });
                }
            }
        }
        sizes.push(placement.partition_size);
        images.push(PreparedImage {
            label: placement.label,
            partition: placement.partition,
            offset: placement.offset,
            bytes,
            placement: placement.placement,
        });
    }

    check_plan(board.protocol, &layout, &images, &sizes)?;

    let mut erase = Vec::with_capacity(board.erase.len());
    for target in &board.erase {
        let entry = match target {
            EraseTarget::Region { offset, size } => PreparedErase {
                label: format!("region at 0x{offset:x}"),
                partition: None,
                region: Region {
                    offset: *offset,
                    size: *size,
                },
            },
            EraseTarget::Partition(name) => {
                let partition = find_partition(need_layout(&layout, board, name)?, board, name)?;
                PreparedErase {
                    label: name.clone(),
                    partition: Some(name.clone()),
                    region: Region {
                        offset: partition.offset,
                        size: partition.size,
                    },
                }
            }
        };
        check_erase(board.protocol, &layout, &entry)?;
        erase.push(entry);
    }

    // What is tied to an offset cannot follow the flash of the chip (R9).
    if let Some(resolved) = &layout {
        let follows_flash = resolved
            .partitions
            .iter()
            .any(|partition| partition.grows_with_flash || partition.moves_with_flash);
        if let (true, Some((_, partition, _))) = (follows_flash, &table_image) {
            return Err(Error::PartitionTableImageForDetectedFlash {
                partition: partition.clone(),
            });
        }
        check_fixed_spans(resolved, &fixed_spans(board, &images, &erase))?;
    }

    Ok(Prepared {
        layout,
        definition,
        images,
        erase,
        warnings,
    })
}

/// Places `prepared` again on a flash of `flash_size` bytes: resolves its layout for that size,
/// moves the images and erase entries that name a partition to where it now is, and generates
/// the partition table again. Images and erase entries given by offset stay, and every check of
/// [`prepare`] runs again on the new places, before anything is erased. The changes are against
/// the layout of `prepared`.
pub(crate) fn place_for_flash(
    prepared: Prepared,
    board: &Board,
    flash_size: u64,
) -> Result<(Prepared, LayoutChanges), Error> {
    let Prepared {
        layout: before,
        definition,
        mut images,
        mut erase,
        warnings,
    } = prepared;
    let Some(definition) = definition else {
        let prepared = Prepared {
            layout: before,
            definition,
            images,
            erase,
            warnings,
        };
        return Ok((prepared, LayoutChanges::default()));
    };
    let resolved = resolve(&definition, Some(flash_size))?;
    let changes = before
        .as_ref()
        .map(|before| resolved.changes_from(before))
        .unwrap_or_default();
    let mut sizes = Vec::with_capacity(images.len());
    for image in &mut images {
        let partition = match &image.partition {
            Some(name) => Some(find_partition(&resolved, board, name)?),
            None => None,
        };
        if image.placement == ImagePlacement::GeneratedTable {
            image.bytes = write(&resolved).map_err(|error| Error::Table { error })?;
        }
        if let (ImagePlacement::Partition | ImagePlacement::GeneratedTable, Some(partition)) =
            (image.placement, partition)
        {
            image.offset = partition.offset;
        }
        sizes.push(partition.map(|partition| partition.size));
    }
    for entry in &mut erase {
        if let Some(name) = &entry.partition {
            let partition = find_partition(&resolved, board, name)?;
            entry.region = Region {
                offset: partition.offset,
                size: partition.size,
            };
        }
    }
    let layout = Some(resolved);
    check_plan(board.protocol, &layout, &images, &sizes)?;
    for entry in &erase {
        check_erase(board.protocol, &layout, entry)?;
    }
    if let Some(resolved) = &layout {
        check_fixed_spans(resolved, &fixed_spans(board, &images, &erase))?;
    }
    let prepared = Prepared {
        layout,
        definition: Some(definition),
        images,
        erase,
        warnings,
    };
    Ok((prepared, changes))
}

/// Merges the images of a board into one flash image, `0xFF` between them.
///
/// The board is the one named `board_identifier`, or the only one of the manifest. The image ends
/// at the highest image, or at the flash size of the layout with `fill`. It never goes past the
/// flash of the layout, nor past what the protocol can address (4 GiB for Espressif).
pub async fn image_build<S: ImageSource>(
    manifest: &Manifest,
    board_identifier: Option<&str>,
    source: &mut S,
    verifier: &mut dyn ImageVerifier,
    fill: bool,
) -> Result<Vec<u8>, Error> {
    let identifiers = || {
        manifest
            .boards
            .iter()
            .map(|board| board.identifier.clone())
            .collect::<Vec<_>>()
    };
    let board = match (board_identifier, manifest.boards.as_slice()) {
        (Some(identifier), boards) => boards
            .iter()
            .find(|board| board.identifier == identifier)
            .ok_or_else(|| Error::UnknownBoard {
                identifier: identifier.to_string(),
                known: identifiers(),
            })?,
        (None, [board]) => board,
        (None, _) => {
            return Err(Error::BoardRequired {
                boards: identifiers(),
            });
        }
    };
    let prepared = prepare(board, source, verifier).await?;
    let mut end = prepared.images.iter().fold(0u64, |end, image| {
        end.max(
            image
                .offset
                .saturating_add(u64::try_from(image.bytes.len()).unwrap_or(u64::MAX)),
        )
    });
    if fill {
        let layout = prepared.layout.as_ref().ok_or(Error::UnknownFlashSize)?;
        end = end.max(layout.flash.size);
    }
    // `prepare` keeps every image inside the flash of the layout and the address range of the
    // protocol; the flash of a layout may still be larger than that range.
    let limit = address_limit(board.protocol);
    if end > limit {
        return Err(Error::OutsideAddressRange {
            what: "flash image".to_string(),
            end,
            limit,
            protocol: board.protocol,
        });
    }
    let out_of_memory = || Error::OutOfMemory {
        what: "flash image".to_string(),
        size: end,
    };
    let length = usize::try_from(end).map_err(|_| out_of_memory())?;
    let mut merged = Vec::new();
    merged
        .try_reserve_exact(length)
        .map_err(|_| out_of_memory())?;
    merged.resize(length, 0xFFu8);
    for image in &prepared.images {
        let range = usize::try_from(image.offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(image.bytes.len())?));
        if let Some(target) = range.and_then(|range| merged.get_mut(range)) {
            target.copy_from_slice(&image.bytes);
        }
    }
    Ok(merged)
}

#[cfg(all(test, feature = "testing", feature = "json"))]
mod tests {
    use super::*;
    use crate::testing::MemorySource;
    use alloc::collections::BTreeMap;
    use pollster::block_on;

    const BOARD: &str = include_str!("../../tests/boards/esp32-s3-usb.toml");
    const LAYOUT: &str = include_str!("../../tests/layouts/esp32-s3-8mb.toml");
    const BOOTLOADER: &[u8] = include_bytes!("../../tests/images/bootloader.bin");
    const APPLICATION: &[u8] = include_bytes!("../../tests/images/application.bin");

    fn fixture_source() -> MemorySource {
        let mut files = BTreeMap::new();
        files.insert(
            "../layouts/esp32-s3-8mb.toml".to_string(),
            LAYOUT.as_bytes().to_vec(),
        );
        files.insert("../images/bootloader.bin".to_string(), BOOTLOADER.to_vec());
        files.insert(
            "../images/application.bin".to_string(),
            APPLICATION.to_vec(),
        );
        MemorySource { files }
    }

    /// A board of the fixture layout with the given `[[board.image]]` and erase text.
    fn board(extra: &str) -> Board {
        let text = format!(
            "[[board]]\nidentifier = \"b\"\nprotocol = \"espressif\"\nchip = \"esp32-s3\"\nlayout = \"../layouts/esp32-s3-8mb.toml\"\n{extra}"
        );
        Manifest::from_toml(&text).unwrap().boards.remove(0)
    }

    fn offset_image(offset: &str, path: &str) -> String {
        format!("[[board.image]]\noffset = {offset}\npath = \"{path}\"\n")
    }

    fn prepare_with(board: &Board, source: &mut MemorySource) -> Result<Prepared, Error> {
        block_on(prepare(board, source, &mut ()))
    }

    #[test]
    fn prepares_the_fixture_board() {
        let manifest = Manifest::from_toml(BOARD).unwrap();
        let prepared = prepare_with(&manifest.boards[0], &mut fixture_source()).unwrap();
        let summary: Vec<(&str, Option<&str>, u64, usize)> = prepared
            .images
            .iter()
            .map(|image| {
                (
                    image.label.as_str(),
                    image.partition.as_deref(),
                    image.offset,
                    image.bytes.len(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("bootloader", Some("bootloader"), 0x0, 4096),
                ("partition-table", Some("partition-table"), 0x8000, 0xC00),
                ("application", Some("application"), 0x20000, 8192),
            ]
        );
        assert_eq!(prepared.images[1].bytes[..4], [0xAA, 0x50, 0x01, 0x02]);
        assert!(prepared.warnings.is_empty());
        assert!(prepared.layout.is_some());
        assert!(prepared.erase.is_empty());
    }

    #[test]
    fn table_image_wins_and_warns_when_different() {
        let different = board(&offset_image("0x8000", "table.bin"));
        let mut source = fixture_source();
        source
            .files
            .insert("table.bin".into(), alloc::vec![0; 0xC00]);
        let prepared = prepare_with(&different, &mut source).unwrap();
        assert_eq!(prepared.images.len(), 1);
        assert_eq!(
            prepared.images[0].partition.as_deref(),
            Some("partition-table")
        );
        assert_eq!(
            prepared.warnings,
            [Warning::PartitionTableDiffers {
                partition: "partition-table".into()
            }]
        );
        assert!(prepared.warnings[0].to_string().contains("differs"));

        // The generated table, padded with 0xFF to a sector, is not different.
        let generated = prepare_with(
            &board(&offset_image("0x20000", "../images/application.bin")),
            &mut fixture_source(),
        )
        .unwrap()
        .images[0]
            .bytes
            .clone();
        let mut padded = generated.clone();
        padded.resize(0x1000, 0xFF);
        source.files.insert("table.bin".into(), padded);
        assert!(
            prepare_with(&different, &mut source)
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn plan_errors() {
        let mut source = fixture_source();
        source
            .files
            .insert("big.bin".into(), alloc::vec![0; 0x1001]);
        source.files.insert("small.bin".into(), alloc::vec![0; 16]);
        source
            .files
            .insert("table-big.bin".into(), alloc::vec![0; 0x8001]);
        source
            .files
            .insert("two-sectors.bin".into(), alloc::vec![0; 0x2000]);

        let misaligned = board(&offset_image("0x1800", "small.bin"));
        assert_eq!(
            prepare_with(&misaligned, &mut source).unwrap_err(),
            Error::MisalignedImage {
                label: "image at 0x1800".into(),
                offset: 0x1800,
                required: 0x1000
            }
        );
        let message = prepare_with(&misaligned, &mut source)
            .unwrap_err()
            .to_string();
        assert!(message.contains("image at 0x1800") && message.contains("0x1800"));

        let overlapping = board(&format!(
            "{}{}",
            offset_image("0x0", "big.bin"),
            offset_image("0x1000", "small.bin")
        ));
        assert_eq!(
            prepare_with(&overlapping, &mut source).unwrap_err(),
            Error::OverlappingImages {
                first: "image at 0x0".into(),
                second: "image at 0x1000".into(),
                first_end: 0x2000,
                second_offset: 0x1000
            }
        );

        let too_large =
            board("[[board.image]]\npartition = \"bootloader\"\npath = \"table-big.bin\"\n");
        assert_eq!(
            prepare_with(&too_large, &mut source).unwrap_err(),
            Error::ImageTooLarge {
                label: "bootloader".into(),
                partition: "bootloader".into(),
                image_size: 0x8001,
                partition_size: 0x8000
            }
        );

        let outside = board(&offset_image("0x7FF000", "two-sectors.bin"));
        assert_eq!(
            prepare_with(&outside, &mut source).unwrap_err(),
            Error::ImageOutsideFlash {
                label: "image at 0x7ff000".into(),
                end: 0x801000,
                flash_size: 0x800000
            }
        );

        let unknown = board("[[board.image]]\npartition = \"nope\"\npath = \"small.bin\"\n");
        assert_eq!(
            prepare_with(&unknown, &mut source).unwrap_err(),
            Error::UnknownPartition {
                board: "b".into(),
                partition: "nope".into()
            }
        );

        let mut without_layout =
            board("[[board.image]]\npartition = \"bootloader\"\npath = \"small.bin\"\n");
        without_layout.layout = None;
        assert_eq!(
            prepare_with(&without_layout, &mut source).unwrap_err(),
            Error::ImageNeedsLayout {
                board: "b".into(),
                partition: "bootloader".into()
            }
        );

        let wrong_digest = board(&format!(
            "[[board.image]]\noffset = 0\npath = \"small.bin\"\nsha256 = \"{}\"\n",
            "0".repeat(64)
        ));
        let Error::ChecksumMismatch {
            path,
            expected,
            actual,
        } = prepare_with(&wrong_digest, &mut source).unwrap_err()
        else {
            panic!("expected a checksum mismatch");
        };
        assert_eq!(path, "small.bin");
        assert_eq!(expected, "0".repeat(64));
        assert_eq!(actual.len(), 64);
        assert!(
            actual
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );

        struct Unsigned;
        impl ImageVerifier for Unsigned {
            fn verify(&mut self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
                Err("unsigned".into())
            }
        }
        let rejected = board(&offset_image("0x0", "small.bin"));
        let error = block_on(prepare(&rejected, &mut source, &mut Unsigned)).unwrap_err();
        assert_eq!(
            error,
            Error::ImageRejected {
                path: "small.bin".into(),
                reason: "unsigned".into()
            }
        );
        assert!(error.to_string().contains("unsigned") && error.to_string().contains("small.bin"));

        let missing = board(&offset_image("0x0", "missing.bin"));
        assert_eq!(
            prepare_with(&missing, &mut source).unwrap_err(),
            Error::ImageSource {
                path: "missing.bin".into(),
                message: "no such file".into()
            }
        );
    }

    #[test]
    fn an_ungeneratable_table_only_fails_without_a_table_image() {
        let mut source = fixture_source();
        source.files.insert(
            "../layouts/esp32-s3-8mb.toml".into(),
            LAYOUT
                .replace("subtype = \"nvs\"", "subtype = \"bogus\"")
                .into_bytes(),
        );
        source
            .files
            .insert("table.bin".into(), alloc::vec![0xFF; 0xC00]);
        let replaced =
            prepare_with(&board(&offset_image("0x8000", "table.bin")), &mut source).unwrap();
        assert!(matches!(
            replaced.warnings.as_slice(),
            [Warning::PartitionTableNotChecked { partition, .. }] if partition == "partition-table"
        ));
        assert!(matches!(
            prepare_with(
                &board(&offset_image("0x20000", "../images/application.bin")),
                &mut source
            ),
            Err(Error::Table { .. })
        ));
    }

    #[test]
    fn samba_images_start_on_4_kib() {
        let mut source = fixture_source();
        source.files.insert("small.bin".into(), alloc::vec![0; 16]);
        let mut samba = board(&offset_image("0x1800", "small.bin"));
        samba.protocol = Protocol::Samba;
        samba.layout = None;
        let error = prepare_with(&samba, &mut source).unwrap_err();
        assert_eq!(
            error,
            Error::MisalignedImage {
                label: "image at 0x1800".into(),
                offset: 0x1800,
                required: 0x1000,
            }
        );
        let message = error.to_string();
        assert!(
            message.contains("the bootloader erases whole sectors of 0x1000 bytes"),
            "{message}"
        );
        let mut aligned = board(&offset_image("0x3000", "small.bin"));
        aligned.protocol = Protocol::Samba;
        aligned.layout = None;
        assert!(prepare_with(&aligned, &mut source).is_ok());
    }

    /// A board of `protocol` without a layout, with one image at `offset` of `size` bytes.
    fn bare(protocol: Protocol, offset: &str, size: usize) -> (Board, MemorySource) {
        let mut source = fixture_source();
        source
            .files
            .insert("small.bin".into(), alloc::vec![0; size]);
        let mut avr = board(&offset_image(offset, "small.bin"));
        avr.protocol = protocol;
        avr.layout = None;
        (avr, source)
    }

    #[test]
    fn avr_images_start_on_a_page() {
        let (mut v1, mut source) = bare(Protocol::Stk500v1, "0x40", 16);
        assert_eq!(
            prepare_with(&v1, &mut source).unwrap_err(),
            Error::MisalignedImage {
                label: "image at 0x40".into(),
                offset: 0x40,
                required: 0x80,
            }
        );
        v1.images[0].target = ImageTarget::Offset(0x80);
        assert!(prepare_with(&v1, &mut source).is_ok());
        let (v2, mut source) = bare(Protocol::Stk500v2, "0x80", 16);
        assert_eq!(
            prepare_with(&v2, &mut source).unwrap_err(),
            Error::MisalignedImage {
                label: "image at 0x80".into(),
                offset: 0x80,
                required: 0x100,
            }
        );
    }

    #[test]
    fn avr_addresses_stop_at_128_kib() {
        let (v1, mut source) = bare(Protocol::Stk500v1, "0x1FF80", 0x81);
        assert_eq!(
            prepare_with(&v1, &mut source).unwrap_err(),
            Error::OutsideAddressRange {
                what: "image \"image at 0x1ff80\"".into(),
                end: 0x2_0001,
                limit: 0x2_0000,
                protocol: Protocol::Stk500v1,
            }
        );
        let (v2, mut source) = bare(Protocol::Stk500v2, "0x1FF00", 0x101);
        assert!(prepare_with(&v2, &mut source).is_ok());
    }

    #[test]
    fn avr_addresses_reach_the_word_limit() {
        assert_eq!(address_limit(Protocol::Stk500v1), 1 << 17);
        assert_eq!(address_limit(Protocol::Avr109), 1 << 17);
        assert_eq!(address_limit(Protocol::Stk500v2), 1 << 32);
    }

    #[test]
    fn layout_problems_are_reported() {
        let mut source = fixture_source();
        let board = board(&offset_image("0x20000", "../images/application.bin"));
        source.files.insert(
            "../layouts/esp32-s3-8mb.toml".into(),
            alloc::vec![0xFF, 0xFE],
        );
        assert!(matches!(
            prepare_with(&board, &mut source),
            Err(Error::LayoutFile { .. })
        ));
        source
            .files
            .insert("../layouts/esp32-s3-8mb.toml".into(), b"flash = 3".to_vec());
        assert!(matches!(
            prepare_with(&board, &mut source),
            Err(Error::LayoutFile { .. })
        ));
        source.files.insert(
            "../layouts/esp32-s3-8mb.toml".into(),
            LAYOUT
                .replace("size = \"8 MiB\"", "size = \"64 KiB\"")
                .into_bytes(),
        );
        assert!(matches!(
            prepare_with(&board, &mut source),
            Err(Error::Layout { .. })
        ));
        source.files.clear();
        assert!(matches!(
            prepare_with(&board, &mut source),
            Err(Error::LayoutFile { .. })
        ));
    }

    #[test]
    fn erase_entries_become_regions() {
        let with_erase = board(
            "[[board.erase]]\npartition = \"nvs\"\n[[board.erase]]\noffset = 0x10000\nsize = 0x2000\n",
        );
        let prepared = prepare_with(&with_erase, &mut fixture_source()).unwrap();
        assert_eq!(
            prepared.erase,
            [
                PreparedErase {
                    label: "nvs".into(),
                    partition: Some("nvs".into()),
                    region: Region {
                        offset: 0x9000,
                        size: 0x6000
                    }
                },
                PreparedErase {
                    label: "region at 0x10000".into(),
                    partition: None,
                    region: Region {
                        offset: 0x10000,
                        size: 0x2000
                    }
                }
            ]
        );
        let unknown = board("[[board.erase]]\npartition = \"nope\"\n");
        assert!(matches!(
            prepare_with(&unknown, &mut fixture_source()),
            Err(Error::UnknownPartition { .. })
        ));
    }

    fn erase(offset: &str, size: &str) -> String {
        format!("[[board.erase]]\noffset = {offset}\nsize = {size}\n")
    }

    #[test]
    fn erase_entries_are_checked_like_images() {
        let misaligned =
            prepare_with(&board(&erase("0x9800", "0x100")), &mut fixture_source()).unwrap_err();
        assert_eq!(
            misaligned,
            Error::MisalignedErase {
                entry: "region at 0x9800".into(),
                offset: 0x9800,
                size: 0x100,
                required: 0x1000
            }
        );
        let message = misaligned.to_string();
        assert!(
            message.contains("\"region at 0x9800\"") && message.contains("0x9800..0x9900"),
            "{message}"
        );
        assert_eq!(misaligned.class(), crate::ErrorClass::Input);
        // The end must be aligned too: the sector it ends in would be erased whole.
        assert!(matches!(
            prepare_with(&board(&erase("0x10000", "0x100")), &mut fixture_source()),
            Err(Error::MisalignedErase {
                offset: 0x10000,
                ..
            })
        ));
        assert_eq!(
            prepare_with(&board(&erase("0x7FF000", "0x2000")), &mut fixture_source()).unwrap_err(),
            Error::EraseOutsideFlash {
                entry: "region at 0x7ff000".into(),
                end: 0x801000,
                flash_size: 0x800000
            }
        );
        let mut without_layout = board(&erase("0xFFFFF000", "0x2000"));
        without_layout.layout = None;
        assert_eq!(
            prepare_with(&without_layout, &mut fixture_source()).unwrap_err(),
            Error::OutsideAddressRange {
                what: "erase entry \"region at 0xfffff000\"".into(),
                end: 0x1_0000_1000,
                limit: 0x1_0000_0000,
                protocol: Protocol::Espressif
            }
        );
        // A partition erase entry goes through the same checks.
        let mut source = fixture_source();
        source.files.insert(
            "../layouts/esp32-s3-8mb.toml".into(),
            LAYOUT
                .replace("size = \"24 KiB\"", "size = \"23 KiB\"")
                .replace("alignment = \"4 KiB\"", "alignment = \"1 KiB\"")
                .into_bytes(),
        );
        // The table of this layout cannot be generated: an image replaces it.
        source
            .files
            .insert("table.bin".into(), alloc::vec![0xFF; 0xC00]);
        let nvs = board(&format!(
            "{}[[board.erase]]\npartition = \"nvs\"\n",
            offset_image("0x8000", "table.bin")
        ));
        assert!(
            matches!(
                prepare_with(&nvs, &mut source),
                Err(Error::MisalignedErase { ref entry, .. }) if entry == "nvs"
            ),
            "{:?}",
            prepare_with(&nvs, &mut source)
        );
    }

    #[test]
    fn empty_boards_and_images_are_refused() {
        let empty = board("");
        assert_eq!(
            prepare_with(&empty, &mut fixture_source()).unwrap_err(),
            Error::NothingToFlash { board: "b".into() }
        );
        let mut source = fixture_source();
        source.files.insert("empty.bin".into(), Vec::new());
        let error =
            prepare_with(&board(&offset_image("0x20000", "empty.bin")), &mut source).unwrap_err();
        assert_eq!(
            error,
            Error::EmptyImage {
                path: "empty.bin".into()
            }
        );
        assert_eq!(error.class(), crate::ErrorClass::Input);
        // An esp-web-tools build without parts has nothing to flash either.
        let manifest =
            Manifest::from_json(r#"{"builds":[{"chipFamily":"ESP32","parts":[]}]}"#).unwrap();
        assert!(matches!(
            prepare_with(&manifest.boards[0], &mut source),
            Err(Error::NothingToFlash { .. })
        ));
    }

    #[test]
    fn image_build_refuses_offsets_beyond_the_address_range() {
        for (offset, end) in [
            (0x100_0000_0000_u64, 0x100_0000_0010_u64),
            (0xFFFF_FFFF_FFFF_0000, 0xFFFF_FFFF_FFFF_0010),
        ] {
            let text = format!(
                r#"{{"builds":[{{"chipFamily":"ESP32","parts":[{{"path":"small.bin","offset":{offset}}}]}}]}}"#
            );
            let manifest = Manifest::from_json(&text).unwrap();
            let mut source = fixture_source();
            source.files.insert("small.bin".into(), alloc::vec![0; 16]);
            let error =
                block_on(image_build(&manifest, None, &mut source, &mut (), false)).unwrap_err();
            assert_eq!(
                error,
                Error::OutsideAddressRange {
                    what: format!("image \"image at 0x{offset:x}\""),
                    end,
                    limit: 1 << 32,
                    protocol: Protocol::Espressif
                }
            );
            assert_eq!(error.class(), crate::ErrorClass::Input);
        }
    }

    #[test]
    fn image_build_merges_with_0xff() {
        let manifest = Manifest::from_toml(BOARD).unwrap();
        let merged = block_on(image_build(
            &manifest,
            None,
            &mut fixture_source(),
            &mut (),
            false,
        ))
        .unwrap();
        assert_eq!(merged.len(), 0x20000 + 8192);
        assert_eq!(&merged[..4096], BOOTLOADER);
        assert!(merged[0x1000..0x8000].iter().all(|byte| *byte == 0xFF));
        assert_eq!(merged[0x8000], 0xAA);
        assert!(merged[0x8C00..0x20000].iter().all(|byte| *byte == 0xFF));
        assert_eq!(&merged[0x20000..], APPLICATION);

        let filled = block_on(image_build(
            &manifest,
            Some("esp32-s3-devkitc"),
            &mut fixture_source(),
            &mut (),
            true,
        ))
        .unwrap();
        assert_eq!(filled.len(), 8 * 1024 * 1024);
        assert_eq!(&filled[..merged.len()], merged.as_slice());

        assert_eq!(
            block_on(image_build(
                &manifest,
                Some("x"),
                &mut fixture_source(),
                &mut (),
                false
            ))
            .unwrap_err(),
            Error::UnknownBoard {
                identifier: "x".into(),
                known: alloc::vec!["esp32-s3-devkitc".into()]
            }
        );
        let mut no_layout = manifest.clone();
        no_layout.boards[0].layout = None;
        no_layout.boards[0].images[0].target = ImageTarget::Offset(0);
        no_layout.boards[0].images[1].target = ImageTarget::Offset(0x20000);
        assert_eq!(
            block_on(image_build(
                &no_layout,
                None,
                &mut fixture_source(),
                &mut (),
                true
            ))
            .unwrap_err(),
            Error::UnknownFlashSize
        );
        let several = Manifest {
            boards: alloc::vec![manifest.boards[0].clone(), {
                let mut other = manifest.boards[0].clone();
                other.identifier = "other".into();
                other
            }],
            prompt_erase: false,
        };
        assert!(matches!(
            block_on(image_build(
                &several,
                None,
                &mut fixture_source(),
                &mut (),
                false
            )),
            Err(Error::BoardRequired { .. })
        ));
    }

    const HEX: &str = include_str!("../../tests/images/avr-application.hex");
    const UNO: &str = include_str!("../../tests/layouts/uno.toml");

    fn hex_source(layout: &str) -> MemorySource {
        let mut source = fixture_source();
        source
            .files
            .insert("../layouts/uno.toml".into(), layout.as_bytes().to_vec());
        source
            .files
            .insert("avr-application.hex".into(), HEX.as_bytes().to_vec());
        source
    }

    fn hex_board(offset: &str, extra: &str) -> Board {
        let text = format!(
            "[[board]]\nidentifier = \"u\"\nprotocol = \"espressif\"\nchip = \"esp32\"\nlayout = \"../layouts/uno.toml\"\n[[board.image]]\noffset = {offset}\npath = \"avr-application.hex\"\n{extra}"
        );
        Manifest::from_toml(&text).unwrap().boards.remove(0)
    }

    #[test]
    fn hex_images_are_placed_where_they_start() {
        let prepared = prepare_with(&hex_board("0", ""), &mut hex_source(UNO)).unwrap();
        assert_eq!(prepared.images.len(), 1);
        assert_eq!(prepared.images[0].bytes.len(), 300);
        assert_eq!(prepared.images[0].offset, 0);

        // A valid file at the wrong offset is not an invalid file: nothing to rebuild.
        let error = prepare_with(&hex_board("0x1000", ""), &mut hex_source(UNO)).unwrap_err();
        assert_eq!(
            error,
            Error::MisplacedIntelHex {
                path: "avr-application.hex".into(),
                start: 0,
                expected: 0x1000,
                base_address: 0,
                partition: None,
            }
        );
        assert_eq!(
            error.to_string(),
            "the Intel HEX image \"avr-application.hex\" starts at 0x0, but the manifest places it at 0x1000: fix the offset of the image to 0x0, or name the partition that starts there"
        );
        // Written to a partition that starts elsewhere.
        let mut board = hex_board("0", "");
        board.images[0].target = crate::manifest::ImageTarget::Partition("bootloader".into());
        let error = prepare_with(&board, &mut hex_source(UNO)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the Intel HEX image \"avr-application.hex\" starts at 0x0, but the partition \"bootloader\" it is written to starts at 0x7e00: fix the partition of the image (it starts at the flash offset 0x0), or rebuild it for that partition"
        );
        // The digest is the one of the file, not of the decoded bytes.
        let file = "c4ba8536e262509cb35fbe5da801c89d3695d5f5d46c99cccae49aae6b8152da";
        let decoded = "b1b36f51f514d11c6cd3a9656327f180209d503961ff0616db46cf88921deb28";
        let accepted = hex_board("0", &format!("sha256 = \"{file}\"\n"));
        assert!(prepare_with(&accepted, &mut hex_source(UNO)).is_ok());
        let refused = hex_board("0", &format!("sha256 = \"{decoded}\"\n"));
        assert!(matches!(
            prepare_with(&refused, &mut hex_source(UNO)),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn hex_addresses_include_the_base_address_of_the_flash() {
        let layout = UNO.replace("base-address = 0x0", "base-address = 0x1000");
        let error = prepare_with(&hex_board("0", ""), &mut hex_source(&layout)).unwrap_err();
        assert_eq!(
            error,
            Error::MisplacedIntelHex {
                path: "avr-application.hex".into(),
                start: 0,
                expected: 0x1000,
                base_address: 0x1000,
                partition: Some("application".into()),
            }
        );
        // Below the base address there is no offset to suggest.
        let text = error.to_string();
        assert!(
            text.ends_with("it starts below the flash, which starts at 0x1000: rebuild it for this board, or fix the base address of the layout"),
            "{text}"
        );
        assert!(!text.contains("offset of the image"), "{text}");
        let mut source = hex_source(&layout);
        source.files.insert(
            "avr-application.hex".into(),
            b":020000040000FA\n:0100000055AA\n:00000001FF\n".to_vec(),
        );
        assert!(prepare_with(&hex_board("0", ""), &mut source).is_err());
    }

    #[test]
    fn eeprom_images_are_refused() {
        let mut source = hex_source(UNO);
        source
            .files
            .insert("sketch.eep".into(), HEX.as_bytes().to_vec());
        let board = Manifest::from_toml(
            "[[board]]\nidentifier = \"u\"\nprotocol = \"espressif\"\nchip = \"esp32\"\n[[board.image]]\noffset = 0\npath = \"sketch.eep\"\n",
        )
        .unwrap()
        .boards
        .remove(0);
        match prepare_with(&board, &mut source) {
            Err(Error::ImageRejected { reason, .. }) => assert!(reason.contains("EEPROM")),
            other => panic!("{other:?}"),
        }
    }

    const DETECT: &str = include_str!("../../tests/layouts/esp32-s3-detect.toml");

    /// The detect layout of the fixtures with a crash-record partition after `storage`, which
    /// moves with the flash: at 0x3F0000 on the minimum of 4 MiB.
    fn moving_layout() -> String {
        format!(
            "{DETECT}\n[[partition]]\nname = \"coredump\"\nrole = \"crash-record\"\nsize = \"64 KiB\"\n"
        )
    }

    /// A source with the moving layout as `m.toml`, the detect layout, and small images.
    fn moving_source() -> MemorySource {
        let mut source = fixture_source();
        source
            .files
            .insert("m.toml".into(), moving_layout().into_bytes());
        source.files.insert(
            "../layouts/esp32-s3-detect.toml".into(),
            DETECT.as_bytes().to_vec(),
        );
        source.files.insert("small.bin".into(), alloc::vec![0; 16]);
        source
    }

    /// An Espressif board of the layout at `layout` with the given images and erase entries.
    fn board_of(layout: &str, extra: &str) -> Board {
        let text = format!(
            "[[board]]\nidentifier = \"b\"\nprotocol = \"espressif\"\nchip = \"esp32-s3\"\nlayout = \"{layout}\"\n{extra}"
        );
        Manifest::from_toml(&text).unwrap().boards.remove(0)
    }

    #[test]
    fn prepare_says_how_inputs_were_placed() {
        let board = board_of(
            "m.toml",
            "[[board.image]]\npartition = \"coredump\"\npath = \"small.bin\"\n\
             [[board.image]]\noffset = 0x20000\npath = \"small.bin\"\n\
             [[board.erase]]\npartition = \"coredump\"\n\
             [[board.erase]]\noffset = 0x9000\nsize = 0x1000\n",
        );
        let prepared = prepare_with(&board, &mut moving_source()).unwrap();
        let placements: Vec<(&str, ImagePlacement)> = prepared
            .images
            .iter()
            .map(|image| (image.label.as_str(), image.placement))
            .collect();
        assert_eq!(
            placements,
            [
                ("partition-table", ImagePlacement::GeneratedTable),
                ("coredump", ImagePlacement::Partition),
                ("image at 0x20000", ImagePlacement::Offset),
            ]
        );
        let erase: Vec<Option<&str>> = prepared
            .erase
            .iter()
            .map(|entry| entry.partition.as_deref())
            .collect();
        assert_eq!(erase, [Some("coredump"), None]);
        assert_eq!(
            prepared.definition,
            Some(Layout::from_toml(&moving_layout()).unwrap())
        );
    }

    #[test]
    fn place_for_flash_moves_partitions_and_regenerates_the_table() {
        let board = board_of(
            "m.toml",
            "[[board.image]]\npartition = \"coredump\"\npath = \"small.bin\"\n\
             [[board.erase]]\npartition = \"coredump\"\n",
        );
        let mut prepared = prepare_with(&board, &mut moving_source()).unwrap();
        let coredump = |prepared: &Prepared| {
            prepared
                .images
                .iter()
                .find(|image| image.label == "coredump")
                .unwrap()
                .offset
        };
        assert_eq!(coredump(&prepared), 0x3F_0000);
        assert_eq!(
            prepared.erase[0].region,
            Region {
                offset: 0x3F_0000,
                size: 0x1_0000
            }
        );
        let kept = Warning::PartitionTableDiffers {
            partition: "kept".into(),
        };
        prepared.warnings.push(kept.clone());
        let (placed, changes) = place_for_flash(prepared, &board, 0x100_0000).unwrap();
        assert_eq!(coredump(&placed), 0xFF_0000);
        assert_eq!(
            placed.erase[0].region,
            Region {
                offset: 0xFF_0000,
                size: 0x1_0000
            }
        );
        let layout = Layout::from_toml(&moving_layout()).unwrap();
        let resolved = layout.resolve_for(Some(0x100_0000)).unwrap();
        let table = placed
            .images
            .iter()
            .find(|image| image.placement == ImagePlacement::GeneratedTable)
            .unwrap();
        assert_eq!(table.bytes, write(&resolved).unwrap());
        assert_ne!(
            table.bytes,
            write(&layout.resolve().unwrap()).unwrap(),
            "the table of the minimum differs"
        );
        assert_eq!(placed.layout.as_ref(), Some(&resolved));
        assert_eq!(placed.definition, Some(layout));
        assert_eq!(placed.warnings, [kept]);
        assert_eq!(
            changes.grown,
            [crate::layout::GrownPartition {
                name: "storage".into(),
                size_at_minimum: 0xD_0000,
                size: 0xCD_0000,
            }]
        );
        assert_eq!(
            changes.moved,
            [crate::layout::MovedPartition {
                name: "coredump".into(),
                offset_at_minimum: 0x3F_0000,
                offset: 0xFF_0000,
            }]
        );
    }

    #[test]
    fn place_for_flash_keeps_inputs_given_by_offset() {
        let board = board_of(
            "m.toml",
            "[[board.image]]\noffset = 0x320000\npath = \"small.bin\"\n\
             [[board.erase]]\noffset = 0x330000\nsize = 0x1000\n",
        );
        let prepared = prepare_with(&board, &mut moving_source()).unwrap();
        let (placed, _) = place_for_flash(prepared.clone(), &board, 0x80_0000).unwrap();
        assert_eq!(placed.images[1].offset, 0x32_0000);
        assert_eq!(placed.images[1].partition.as_deref(), Some("storage"));
        assert_eq!(placed.erase, prepared.erase);
        // A layout the flash of the chip does not suit fails as a layout.
        assert!(matches!(
            place_for_flash(prepared, &board, 0x80_0800),
            Err(Error::Layout { .. })
        ));
    }

    /// A layout whose last partition leaves a tail on the minimum flash: `storage` grows in steps
    /// of 64 KiB, the alignment of `coredump` and `log` after it, and `log` ends at 0x3F8000.
    const TAIL: &str = "[flash]\nsize = \"detect\"\nminimum = \"4 MiB\"\n\n\
        [[partition]]\nname = \"bootloader\"\nrole = \"bootloader\"\noffset = 0x0\nsize = \"64 KiB\"\n\n\
        [[partition]]\nname = \"application\"\nrole = \"firmware\"\nsize = \"1 MiB\"\nalignment = \"64 KiB\"\n\n\
        [[partition]]\nname = \"storage\"\nrole = \"storage\"\nsize = { minimum = \"64 KiB\" }\n\n\
        [[partition]]\nname = \"coredump\"\nrole = \"crash-record\"\nsize = \"64 KiB\"\nalignment = \"64 KiB\"\n\n\
        [[partition]]\nname = \"log\"\nrole = \"custom\"\nsize = \"32 KiB\"\nalignment = \"64 KiB\"\n";

    #[test]
    fn inputs_fixed_to_moving_partitions_are_refused() {
        let refused = |extra: &str, source: &mut MemorySource| {
            prepare_with(&board_of("m.toml", extra), source).unwrap_err()
        };
        let error = refused(
            "[[board.image]]\noffset = 0x3F0000\npath = \"small.bin\"\n",
            &mut moving_source(),
        );
        assert_eq!(
            error,
            Error::FixedOffsetOnMovingPartition {
                what: "image \"image at 0x3f0000\"".into(),
                start: 0x3F_0000,
                end: 0x3F_0010,
                partition: "coredump".into(),
                partition_offset: 0x3F_0000,
                flash_size: 0x40_0000,
                intel_hex: false,
            }
        );
        assert_eq!(error.class(), crate::ErrorClass::Input);
        assert_eq!(
            error.to_string(),
            "the image \"image at 0x3f0000\" covers 0x3f0000..0x3f0010, which overlaps the partition \"coredump\" at 0x3f0000 on 0x400000 bytes of flash, but \"coredump\" moves with the flash of the chip: target the partition by name in the manifest"
        );
        // A region that starts before the partition and ends inside it.
        assert_eq!(
            refused(
                "[[board.erase]]\noffset = 0x3E0000\nsize = 0x11000\n",
                &mut moving_source()
            ),
            Error::FixedOffsetOnMovingPartition {
                what: "erase entry \"region at 0x3e0000\"".into(),
                start: 0x3E_0000,
                end: 0x3F_1000,
                partition: "coredump".into(),
                partition_offset: 0x3F_0000,
                flash_size: 0x40_0000,
                intel_hex: false,
            }
        );
        assert!(matches!(
            refused(
                "[[board.erase]]\noffset = 0x3F8000\nsize = 0x1000\n",
                &mut moving_source()
            ),
            Error::FixedOffsetOnMovingPartition { ref partition, start: 0x3F_8000, .. }
                if partition == "coredump"
        ));
        // An Intel HEX image carries its addresses, even when given by name.
        let mut source = moving_source();
        source.files.insert(
            "core.hex".into(),
            b":02000004003FBB\n:0100000055AA\n:00000001FF\n".to_vec(),
        );
        let error = refused(
            "[[board.image]]\npartition = \"coredump\"\npath = \"core.hex\"\n",
            &mut source,
        );
        assert_eq!(
            error,
            Error::FixedOffsetOnMovingPartition {
                what: "Intel HEX image \"core.hex\"".into(),
                start: 0x3F_0000,
                end: 0x3F_0001,
                partition: "coredump".into(),
                partition_offset: 0x3F_0000,
                flash_size: 0x40_0000,
                intel_hex: true,
            }
        );
        assert!(
            error
                .to_string()
                .ends_with("moves with the flash of the chip: give the partition a binary image (.bin), targeted by name"),
            "{error}"
        );
        // Inputs given by offset elsewhere, and by name, are placed again on the chip.
        let fine = board_of(
            "m.toml",
            "[[board.image]]\noffset = 0x320000\npath = \"small.bin\"\n\
             [[board.image]]\npartition = \"coredump\"\npath = \"small.bin\"\n\
             [[board.erase]]\noffset = 0x9000\nsize = 0x1000\n",
        );
        assert!(prepare_with(&fine, &mut moving_source()).is_ok());

        // A table image cannot follow the chip.
        let mut source = moving_source();
        source
            .files
            .insert("table.bin".into(), alloc::vec![0xFF; 0xC00]);
        let error = refused(
            "[[board.image]]\noffset = 0x8000\npath = \"table.bin\"\n",
            &mut source,
        );
        assert_eq!(
            error,
            Error::PartitionTableImageForDetectedFlash {
                partition: "partition-table".into()
            }
        );
        assert_eq!(error.class(), crate::ErrorClass::Input);
        assert_eq!(
            error.to_string(),
            "the manifest writes an image to the partition table \"partition-table\", but partitions of the layout grow or move with the flash of the chip, so no fixed table fits every chip: remove the image and let nanoflash generate the table, or give the layout a fixed flash size"
        );
        // By name too; a table image of a fixed layout stays allowed.
        assert!(matches!(
            refused(
                "[[board.image]]\npartition = \"partition-table\"\npath = \"table.bin\"\n",
                &mut source
            ),
            Error::PartitionTableImageForDetectedFlash { .. }
        ));
        assert!(prepare_with(&board(&offset_image("0x8000", "table.bin")), &mut source).is_ok());
    }

    #[test]
    fn inputs_in_the_tail_of_the_minimum_are_checked_again_on_the_chip() {
        let mut source = moving_source();
        source
            .files
            .insert("tail.toml".into(), TAIL.as_bytes().to_vec());
        source
            .files
            .insert("sector.bin".into(), alloc::vec![0; 0x1000]);
        let minimum = Layout::from_toml(TAIL).unwrap().resolve().unwrap();
        assert_eq!(minimum.partition("coredump").unwrap().offset, 0x3E_0000);
        assert_eq!(minimum.partition("log").unwrap().end(), 0x3F_8000);
        for extra in [
            "[[board.image]]\noffset = 0x3F8000\npath = \"sector.bin\"\n",
            "[[board.erase]]\noffset = 0x3F8000\nsize = 0x1000\n",
        ] {
            let mut board = board_of("tail.toml", extra);
            board.protocol = Protocol::Samba;
            // Nothing moves over the tail on the minimum.
            let prepared = prepare_with(&board, &mut source).unwrap();
            // A step of 64 KiB more brings "coredump" over it.
            let error = place_for_flash(prepared.clone(), &board, 0x41_0000).unwrap_err();
            assert!(
                matches!(
                    &error,
                    Error::FixedOffsetOnMovingPartition {
                        partition,
                        start: 0x3F_8000,
                        end: 0x3F_9000,
                        partition_offset: 0x3F_0000,
                        flash_size: 0x41_0000,
                        ..
                    } if partition == "coredump"
                ),
                "{error:?}"
            );
            // On 8 MiB, the tail is in "storage", which grows and does not move.
            assert!(place_for_flash(prepared, &board, 0x80_0000).is_ok());
        }
    }

    #[test]
    fn images_are_checked_against_the_minimum() {
        let mut source = moving_source();
        source
            .files
            .insert("big.bin".into(), alloc::vec![0; 0xE_1000]);
        let board = board_of(
            "../layouts/esp32-s3-detect.toml",
            "[[board.image]]\npartition = \"storage\"\npath = \"big.bin\"\n",
        );
        assert_eq!(
            prepare_with(&board, &mut source).unwrap_err(),
            Error::ImageTooLarge {
                label: "storage".into(),
                partition: "storage".into(),
                image_size: 0xE_1000,
                partition_size: 0xE_0000,
            }
        );
    }

    #[test]
    fn hex_errors_name_the_image_and_the_line() {
        let mut source = hex_source(UNO);
        source
            .files
            .insert("avr-application.hex".into(), b":0100000055AB\n".to_vec());
        let error = prepare_with(&hex_board("0", ""), &mut source).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("\"avr-application.hex\" is invalid at line 1"),
            "{text}"
        );
        assert!(
            text.ends_with("rebuild it, or give its binary form"),
            "{text}"
        );
        let nothing = Error::IntelHex {
            path: "a.hex".into(),
            line: None,
            message: "m".into(),
        };
        assert_eq!(
            nothing.to_string(),
            "the Intel HEX image \"a.hex\" is invalid: m: rebuild it, or give its binary form"
        );
    }
}
