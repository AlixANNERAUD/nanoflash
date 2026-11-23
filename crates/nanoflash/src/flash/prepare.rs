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
use crate::layout::{Layout, ResolvedLayout, Role};
use crate::manifest::{Board, EraseTarget, ImageTarget, Manifest, Protocol};
use crate::table::espressif::{TABLE_SIZE, write};

/// The erase unit of a protocol: images start at a multiple of it, and the sectors an image ends
/// in are erased whole. The Espressif ROM loader erases sectors of 4 KiB; the nRF52840 programs
/// and erases whole 4 KiB pages, which the 256-byte rows of the SAMD21 divide.
fn erase_unit(protocol: Protocol) -> u64 {
    match protocol {
        Protocol::Espressif | Protocol::Samba => 0x1000,
    }
}

/// The end of what a protocol can address: its offsets and sizes are 32-bit words.
pub(crate) fn address_limit(protocol: Protocol) -> u64 {
    match protocol {
        Protocol::Espressif | Protocol::Samba => 1 << 32,
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
                for (index, partition) in partitions.iter().enumerate() {
                    let before = if index == 0 {
                        ", including the partitions "
                    } else {
                        ", "
                    };
                    write!(formatter, "{before}\"{partition}\"")?;
                }
                Ok(())
            }
        }
    }
}

/// A region of flash to erase, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedErase {
    /// The name for messages: the partition, or `region at 0x<offset>`.
    pub label: String,
    /// The region.
    pub region: Region,
}

/// What a board flashes, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The resolved layout of the board, when it has one.
    pub layout: Option<ResolvedLayout>,
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

async fn read_layout<S: ImageSource>(
    board: &Board,
    source: &mut S,
) -> Result<Option<ResolvedLayout>, Error> {
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
    layout
        .resolve()
        .map(Some)
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
    let layout = read_layout(board, source).await?;

    let mut placements: Vec<Placement<'_>> = Vec::new();
    for image in &board.images {
        let origin = Origin::File(&image.path, image.sha256);
        placements.push(match &image.target {
            ImageTarget::Partition(name) => {
                let partition = find_partition(need_layout(&layout, board, name)?, board, name)?;
                Placement {
                    label: name.clone(),
                    partition: Some(name.clone()),
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
                        return Err(Error::IntelHex {
                            path: path.to_string(),
                            line: None,
                            message: format!(
                                "starts at 0x{:x}, and the manifest places it at 0x{expected:x}: set the offset of the image to 0x{:x}, or name the partition that starts there",
                                image.start,
                                image.start.saturating_sub(base)
                            ),
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
        });
    }

    check_plan(board.protocol, &layout, &images, &sizes)?;

    let mut erase = Vec::with_capacity(board.erase.len());
    for target in &board.erase {
        let entry = match target {
            EraseTarget::Region { offset, size } => PreparedErase {
                label: format!("region at 0x{offset:x}"),
                region: Region {
                    offset: *offset,
                    size: *size,
                },
            },
            EraseTarget::Partition(name) => {
                let partition = find_partition(need_layout(&layout, board, name)?, board, name)?;
                PreparedErase {
                    label: name.clone(),
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

    Ok(Prepared {
        layout,
        images,
        erase,
        warnings,
    })
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
                    region: Region {
                        offset: 0x9000,
                        size: 0x6000
                    }
                },
                PreparedErase {
                    label: "region at 0x10000".into(),
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

        let error = prepare_with(&hex_board("0x1000", ""), &mut hex_source(UNO)).unwrap_err();
        match &error {
            Error::IntelHex {
                line: None,
                message,
                ..
            } => {
                assert!(
                    message.contains("0x0") && message.contains("0x1000"),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
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
        match &error {
            Error::IntelHex { message, .. } => {
                assert!(
                    message.contains("starts at 0x0") && message.contains("0x1000"),
                    "{message}"
                )
            }
            other => panic!("{other:?}"),
        }
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
