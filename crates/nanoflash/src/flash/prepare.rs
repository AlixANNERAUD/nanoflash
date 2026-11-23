//! Reading the images of a board, placing them in flash and checking the plan.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{ImageSource, ImageVerifier};
use crate::backend::Region;
use crate::error::Error;
use crate::layout::{Layout, ResolvedLayout, Role};
use crate::manifest::{Board, EraseTarget, ImageTarget, Manifest, Protocol};
use crate::table::espressif::{TABLE_SIZE, write};

/// The erase unit of the Espressif ROM loader: images start at a multiple of it, and the sectors
/// an image ends in are erased whole.
const ERASE_UNIT: u64 = 0x1000;

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
}

impl fmt::Display for Warning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::PartitionTableDiffers { partition } => write!(
                formatter,
                "the image for the partition \"{partition}\" differs from the partition table generated from the layout: the board will boot with the table of the image"
            ),
        }
    }
}

/// What a board flashes, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The resolved layout of the board, when it has one.
    pub layout: Option<ResolvedLayout>,
    /// The images, sorted as the manifest lists them, the generated table by its offset.
    pub images: Vec<PreparedImage>,
    /// The regions to erase first.
    pub erase: Vec<Region>,
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

fn round_up(value: u64) -> u64 {
    value.saturating_add(ERASE_UNIT - 1) & !(ERASE_UNIT - 1)
}

/// The first problem of the plan, checking the images in order, then their neighbours.
fn check_plan(
    layout: &Option<ResolvedLayout>,
    images: &[PreparedImage],
    sizes: &[Option<u64>],
) -> Result<(), Error> {
    for (image, partition_size) in images.iter().zip(sizes) {
        let size = u64::try_from(image.bytes.len()).unwrap_or(u64::MAX);
        if image.offset % ERASE_UNIT != 0 {
            return Err(Error::MisalignedImage {
                label: image.label.clone(),
                offset: image.offset,
                required: ERASE_UNIT,
            });
        }
        if let (Some(partition), Some(partition_size)) = (&image.partition, partition_size)
            && size > *partition_size
        {
            return Err(Error::ImageTooLarge {
                label: image.label.clone(),
                partition: partition.clone(),
                image_size: size,
                partition_size: *partition_size,
            });
        }
        if let Some(layout) = layout {
            let end = image.offset.saturating_add(size);
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
        let first_end = round_up(first.offset.saturating_add(size));
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

/// Reads the layout and the images of `board`, and checks where they go.
///
/// Every image is read through `source`, checked against its digest and handed to `verifier`.
/// When the board speaks the Espressif protocol and its layout has a partition table, no image
/// lands on, the table generated from the layout is added.
pub async fn prepare<S: ImageSource>(
    board: &Board,
    source: &mut S,
    verifier: &mut dyn ImageVerifier,
) -> Result<Prepared, Error> {
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
    // The index of the image that replaces the generated table, with the table.
    let mut table_image: Option<(usize, Vec<u8>)> = None;
    if board.protocol == Protocol::Espressif
        && let Some(layout) = &layout
        && let Some(table) = layout
            .partitions
            .iter()
            .find(|partition| partition.definition.role == Role::PartitionTable)
    {
        let generated = write(layout).map_err(|error| Error::Table { error })?;
        match placements
            .iter()
            .position(|placement| placement.offset == table.offset)
        {
            Some(index) => table_image = Some((index, generated)),
            None => {
                let position = placements
                    .iter()
                    .position(|placement| placement.offset > table.offset)
                    .unwrap_or(placements.len());
                placements.insert(
                    position,
                    Placement {
                        label: table.definition.name.clone(),
                        partition: Some(table.definition.name.clone()),
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
                bytes
            }
        };
        if let Some((table_index, generated)) = &table_image
            && *table_index == index
        {
            let (head, rest) = bytes.split_at(bytes.len().min(TABLE_SIZE));
            if head != generated.as_slice() || rest.iter().any(|byte| *byte != 0xFF) {
                warnings.push(Warning::PartitionTableDiffers {
                    partition: placement.label.clone(),
                });
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

    check_plan(&layout, &images, &sizes)?;

    let mut erase = Vec::with_capacity(board.erase.len());
    for target in &board.erase {
        erase.push(match target {
            EraseTarget::Region { offset, size } => Region {
                offset: *offset,
                size: *size,
            },
            EraseTarget::Partition(name) => {
                let partition = find_partition(need_layout(&layout, board, name)?, board, name)?;
                Region {
                    offset: partition.offset,
                    size: partition.size,
                }
            }
        });
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
/// at the highest image, or at the flash size of the layout with `fill`.
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
    let length = usize::try_from(end).map_err(|_| Error::OutOfRange {
        what: "flash image size".to_string(),
        value: end,
    })?;
    let mut merged = alloc::vec![0xFFu8; length];
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
                partition: "image at 0x8000".into()
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
    fn layout_problems_are_reported() {
        let mut source = fixture_source();
        let board = board("");
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
                Region {
                    offset: 0x9000,
                    size: 0x6000
                },
                Region {
                    offset: 0x10000,
                    size: 0x2000
                }
            ]
        );
        let unknown = board("[[board.erase]]\npartition = \"nope\"\n");
        assert!(matches!(
            prepare_with(&unknown, &mut fixture_source()),
            Err(Error::UnknownPartition { .. })
        ));
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
}
