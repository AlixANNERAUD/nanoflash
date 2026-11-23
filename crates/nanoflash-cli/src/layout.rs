//! `nanoflash layout`: checking a layout and writing the files derived from it.

use crate::Format;
use crate::output::{self, Failure};
use crate::source;
use nanoflash::error::Error;
use nanoflash::flash::{IntelHexError, Warning, decode_intel_hex, is_intel_hex, prepare};
use nanoflash::layout::{ImageSize, Layout, LayoutErrors, ResolvedLayout};
use nanoflash::manifest::{Board, Manifest};
use nanoflash::quantity;
use nanoflash::table::{TableError, espressif, linker, mbr};
use std::fs;
use std::path::Path;

/// Read the layout at `path` and resolve it, for `flash_size` when given (a layout whose flash
/// size is "detect"), else for its own flash size (the minimum of a "detect" layout).
pub fn load(path: &Path, flash_size: Option<&str>) -> Result<ResolvedLayout, Failure> {
    let flash_size = flash_size
        .map(|text| {
            quantity::parse(text)
                .map_err(|error| Failure::Input(format!("bad --flash-size: {error}")))
        })
        .transpose()?;
    let text = fs::read_to_string(path)
        .map_err(|error| Failure::Input(format!("cannot read {}: {error}", path.display())))?;
    let layout = Layout::from_toml(&text)
        .map_err(|error| Failure::Input(format!("{}: {error}", path.display())))?;
    layout
        .resolve_for(flash_size)
        .map_err(|LayoutErrors(errors)| Failure::Layout(errors))
}

/// The size of the image of an `--image` argument: its file, or its decoded span for Intel HEX
/// (`.hex`), which may span at most the flash of `layout` and must start where its partition
/// does (the base address of the flash plus the offset of the partition).
fn image_size(argument: &str, layout: &ResolvedLayout) -> Result<ImageSize, Failure> {
    let Some((partition, file)) = argument.split_once('=') else {
        return Err(Failure::Input(format!(
            "bad --image argument \"{argument}\": expected PARTITION=FILE"
        )));
    };
    if partition.is_empty() || file.is_empty() {
        return Err(Failure::Input(format!(
            "bad --image argument \"{argument}\": expected PARTITION=FILE"
        )));
    }
    let metadata = fs::metadata(file)
        .map_err(|error| Failure::Input(format!("cannot read image {file}: {error}")))?;
    if !metadata.is_file() {
        return Err(Failure::Input(format!(
            "cannot read image {file}: not a regular file"
        )));
    }
    let size = if is_intel_hex(file) {
        let text = fs::read(file)
            .map_err(|error| Failure::Input(format!("cannot read image {file}: {error}")))?;
        let image = decode_intel_hex(&text, layout.flash.size).map_err(|error| {
            Failure::Input(match error {
                IntelHexError::Invalid { line, message } => Error::IntelHex {
                    path: file.to_string(),
                    line,
                    message,
                }
                .to_string(),
                IntelHexError::OutOfMemory { .. } => format!("cannot read image {file}: {error}"),
            })
        })?;
        // An unknown partition is reported with the others by the layout's report.
        let target = layout
            .partitions
            .iter()
            .find(|found| found.definition.name == partition);
        if let Some(target) = target {
            let base_address = layout.flash.base_address;
            let expected = base_address.saturating_add(target.offset);
            if image.start != expected {
                return Err(Failure::Error(Error::MisplacedIntelHex {
                    path: file.to_string(),
                    start: image.start,
                    expected,
                    base_address,
                    partition: Some(partition.to_string()),
                }));
            }
        }
        u64::try_from(image.bytes.len()).unwrap_or(u64::MAX)
    } else {
        metadata.len()
    };
    Ok(ImageSize {
        partition: partition.to_string(),
        size,
    })
}

/// The manifest to check images against, and the board to use.
pub struct ManifestChoice<'a> {
    /// The manifest file.
    pub manifest: &'a Path,
    /// The board, when the manifest has several.
    pub board_identifier: Option<&'a str>,
}

/// The board of `manifest` that uses the layout file at `layout`.
fn board_for_layout<'m>(
    manifest: &'m Manifest,
    directory: &Path,
    layout: &Path,
    identifier: Option<&str>,
) -> Result<&'m Board, Failure> {
    let identifiers = || {
        manifest
            .boards
            .iter()
            .map(|board| board.identifier.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(identifier) = identifier {
        return manifest
            .boards
            .iter()
            .find(|board| board.identifier == identifier)
            .ok_or_else(|| {
                Failure::Input(format!(
                    "the manifest has no board \"{identifier}\": the boards are {}",
                    identifiers()
                ))
            });
    }
    let wanted = fs::canonicalize(layout).ok();
    let uses_layout = |board: &&Board| {
        let Some(path) = &board.layout else {
            return false;
        };
        wanted.is_some() && fs::canonicalize(directory.join(path)).ok() == wanted
    };
    let mut found = manifest.boards.iter().filter(uses_layout);
    match (found.next(), found.next()) {
        (Some(board), None) => Ok(board),
        (None, _) => Err(Failure::Input(format!(
            "no board of the manifest uses the layout {}: the boards are {}; pass --board-identifier",
            layout.display(),
            identifiers()
        ))),
        (Some(_), Some(_)) => {
            let boards = manifest
                .boards
                .iter()
                .filter(uses_layout)
                .map(|board| board.identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Failure::Input(format!(
                "several boards of the manifest use the layout {} ({boards}): pass --board-identifier",
                layout.display()
            )))
        }
    }
}

/// The sizes of the images a manifest board writes, and the warnings of preparing them.
fn manifest_sizes(
    layout: &Path,
    choice: &ManifestChoice<'_>,
) -> Result<(Vec<ImageSize>, Vec<Warning>), Failure> {
    let (manifest, mut source) = source::load_manifest(choice.manifest)?;
    let board = board_for_layout(
        &manifest,
        &source.directory,
        layout,
        choice.board_identifier,
    )?;
    let prepared =
        pollster::block_on(prepare(board, &mut source, &mut ())).map_err(Failure::Error)?;
    let sizes = prepared
        .images
        .iter()
        .filter_map(|image| {
            Some(ImageSize {
                partition: image.partition.clone()?,
                size: u64::try_from(image.bytes.len()).unwrap_or(u64::MAX),
            })
        })
        .collect();
    Ok((sizes, prepared.warnings))
}

pub fn check(
    layout: &Path,
    images: &[String],
    manifest: Option<ManifestChoice<'_>>,
    json: bool,
    flash_size: Option<&str>,
) -> Result<String, Failure> {
    // Images are always checked against the layout on its minimum flash, the sizes `flash`
    // enforces; `--flash-size` only changes the offsets, sizes and headroom that are shown.
    let resolved = load(layout, None)?;
    let mut warnings = None;
    let sizes = match &manifest {
        Some(choice) => {
            let (sizes, found) = manifest_sizes(layout, choice)?;
            warnings = Some(found);
            sizes
        }
        None => images
            .iter()
            .map(|argument| image_size(argument, &resolved))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mut report = resolved
        .report(&sizes)
        .map_err(|LayoutErrors(errors)| Failure::Layout(errors))?;
    if flash_size.is_some() {
        report = load(layout, flash_size)?
            .report(&sizes)
            .map_err(|LayoutErrors(errors)| Failure::Layout(errors))?;
    }
    if !json {
        output::print_warnings(warnings.as_deref().unwrap_or(&[]));
    }
    Ok(if json {
        output::report_json(&report, warnings.as_deref())
    } else {
        output::report_table(&report)
    })
}

pub fn write(
    layout: &Path,
    format: Format,
    destination: &Path,
    json: bool,
    flash_size: Option<&str>,
) -> Result<String, Failure> {
    let resolved = load(layout, flash_size)?;
    let bytes: Vec<u8> = match format {
        Format::Espressif => espressif::write(&resolved),
        Format::Mbr => mbr::write(&resolved).map(|table| table.to_vec()),
        Format::Linker => linker::memory_regions(&resolved).map(String::into_bytes),
        Format::Constants => linker::rust_constants(&resolved).map(String::into_bytes),
    }
    .map_err(|error: TableError| Failure::Table(error))?;
    fs::write(destination, &bytes).map_err(|error| {
        Failure::Input(format!("cannot write {}: {error}", destination.display()))
    })?;
    let file = destination.display().to_string();
    Ok(if json {
        output::written_json(&file, bytes.len())
    } else {
        let kind = match format {
            Format::Espressif | Format::Mbr => "table",
            Format::Linker | Format::Constants => "output",
        };
        format!(
            "wrote {} {kind} to {file} ({} bytes)\n",
            format.name(),
            bytes.len()
        )
    })
}
