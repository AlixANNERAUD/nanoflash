//! Flashing a board from its manifest and detecting a chip, through one code path for every
//! backend: choosing the board a device is, preparing its images, then connecting, erasing,
//! writing, verifying and resetting.

mod intel_hex;
pub(crate) mod prepare;
mod select;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::Serialize;

pub use crate::backend::BackendOptions;
use crate::backend::{
    Backend, Backends, ChipInformation, Progress, ProgressEvent, Region, Segment, Stage,
};
use crate::error::Error;
use crate::layout::{ResolvedLayout, Role};
use crate::manifest::{Board, Manifest, Protocol};
use crate::transport::{DeviceFilter, PortProvider, SerialPort};
pub use intel_hex::{IntelHexError, IntelHexImage, decode_intel_hex, is_intel_hex};
pub use prepare::{
    ImagePlacement, Prepared, PreparedErase, PreparedImage, Warning, image_build,
    image_build_for_flash, prepare,
};
pub use select::{Selection, protocol_for_identity, select};

/// Reads the files a manifest names.
///
/// Paths are exactly as written in the manifest, relative to it or URLs; the source resolves them.
// Futures are not Send: browser APIs are single-threaded (docs/design.md, Constraints).
#[allow(async_fn_in_trait)]
pub trait ImageSource {
    /// The bytes of the file at `path`, or what went wrong.
    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String>;
}

/// Decides whether an image may be flashed, such as by checking a signature.
///
/// It sees the file as read: for an Intel HEX image (`.hex`), the text, not the bytes it decodes
/// to.
pub trait ImageVerifier {
    /// Accepts the image read from `path`, or says why not.
    fn verify(&mut self, path: &str, bytes: &[u8]) -> Result<(), String>;
}

impl ImageVerifier for () {
    fn verify(&mut self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

/// How to flash a board.
#[derive(Debug, Clone)]
pub struct FlashOptions {
    /// The board of the manifest to flash, when the device cannot tell.
    pub board_identifier: Option<String>,
    /// Whether to erase the whole flash before writing: the size the chip reports, else the
    /// flash size of the layout, from the lowest offset the bootloader writes; a bootloader at
    /// the top of the flash (AVR) stops it at `writable_to`, or at its partition of the layout.
    pub erase_all: bool,
    /// Whether to check what was written.
    pub verify: bool,
    /// How to connect.
    pub backend: BackendOptions,
}

impl Default for FlashOptions {
    fn default() -> Self {
        FlashOptions {
            board_identifier: None,
            erase_all: false,
            verify: true,
            backend: BackendOptions::default(),
        }
    }
}

/// What [`flash`] reads images through and reports to.
pub struct Hooks<'h, S: ImageSource> {
    /// Reads the layout and the images the manifest names.
    pub source: &'h mut S,
    /// Accepts or refuses each image read.
    pub verifier: &'h mut dyn ImageVerifier,
    /// Receives progress events and messages.
    pub progress: &'h mut dyn Progress,
}

/// What [`flash`] did.
#[derive(Debug, Clone, Serialize)]
pub struct FlashReport {
    /// The identifier of the board flashed.
    pub board: String,
    /// What the backend found out about the chip.
    pub chip: ChipInformation,
    /// The images written, by offset.
    pub images: Vec<WrittenImage>,
    /// Whether what was written was checked: not when asked not to, nor in secure download mode.
    pub verified: bool,
    /// What the user may want to know.
    pub warnings: Vec<Warning>,
}

/// An image [`flash`] wrote.
#[derive(Debug, Clone, Serialize)]
pub struct WrittenImage {
    /// The name of the image: its partition, or `image at 0x<offset>`.
    pub label: String,
    /// The flash offset of its first byte.
    pub offset: u64,
    /// Its size in bytes.
    pub size: u64,
}

fn report(progress: &mut dyn Progress, stage: Stage, offset: u64, done: u64, total: u64) {
    progress.report(ProgressEvent {
        stage,
        offset,
        done,
        total,
    });
}

/// Connects, reporting `Connecting`.
async fn connect<P: PortProvider>(
    backend: &mut Backends<'_, P>,
    progress: &mut dyn Progress,
) -> Result<ChipInformation, Error> {
    report(progress, Stage::Connecting, 0, 0, 0);
    backend
        .connect(progress)
        .await
        .map_err(|error| error.during(|| "connecting".into()))
}

/// Resets into the application, reporting `Resetting`.
async fn reset<P: PortProvider>(
    backend: &mut Backends<'_, P>,
    progress: &mut dyn Progress,
) -> Result<(), Error> {
    report(progress, Stage::Resetting, 0, 0, 0);
    backend
        .reset_to_application()
        .await
        .map_err(|error| error.during(|| "resetting into the application".into()))
}

/// Erases `region`, reporting `Erasing` before and after.
async fn erase<P: PortProvider>(
    backend: &mut Backends<'_, P>,
    region: Region,
    progress: &mut dyn Progress,
) -> Result<(), Error> {
    report(progress, Stage::Erasing, region.offset, 0, region.size);
    backend.erase(region, progress).await.map_err(|error| {
        error.during(|| {
            format!(
                "erasing 0x{:x}..0x{:x}",
                region.offset,
                region.offset.saturating_add(region.size)
            )
        })
    })?;
    report(
        progress,
        Stage::Erasing,
        region.offset,
        region.size,
        region.size,
    );
    Ok(())
}

/// Flashes the board `manifest` has for the device on `port`, which `provider` can reopen.
///
/// The board is the one `options.board_identifier` names, else the boards that list the USB
/// identity of the port, else those that suit a serial bridge; the chip found picks among them.
/// The images of a single candidate are read and checked before the device is touched. Once
/// connected, the board is reset into its application whether flashing succeeds or not.
pub async fn flash<P: PortProvider, S: ImageSource>(
    manifest: &Manifest,
    port: P::Port,
    provider: &mut P,
    hooks: Hooks<'_, S>,
    options: &FlashOptions,
) -> Result<FlashReport, Error> {
    let Hooks {
        source,
        verifier,
        progress,
    } = hooks;
    let identity = port.identity();
    let selection = select(
        manifest,
        identity.as_ref(),
        options.board_identifier.as_deref(),
    )?;
    let prepared = match selection.boards.as_slice() {
        [board] => Some(prepare(board, source, verifier).await?),
        _ => None,
    };
    let mut backend_options = options.backend.clone();
    if backend_options.bootloader_devices.is_empty() {
        backend_options.bootloader_devices = bootloader_devices(&selection.boards);
    }
    let mut backend = Backends::new(
        selection.protocol,
        port,
        provider,
        selection.reset.clone(),
        &backend_options,
    )?;
    let chip = connect(&mut backend, progress).await?;
    let outcome = flash_connected(
        &mut backend,
        &selection,
        prepared,
        chip,
        Hooks {
            source,
            verifier,
            progress: &mut *progress,
        },
        options,
    )
    .await;
    match outcome {
        Ok(flashed) => {
            reset(&mut backend, progress).await?;
            Ok(flashed)
        }
        Err(error) => {
            // The first error is the one to fix; the failed reset is only told.
            if let Err(reset_error) = reset(&mut backend, progress).await {
                progress.message(&format!(
                    "could not reset into the application: {reset_error}; press the board's reset button"
                ));
            }
            Err(error)
        }
    }
}

/// Resolves the layout of a `detect` board again for the flash the chip reports, with the
/// warnings that tell what changed (R10): a chip below the minimum fails, one that reports no size
/// keeps the minimum, and one of exactly the minimum changes nothing. A layout of a fixed size,
/// or no layout, is left as it is.
fn resolve_for_chip(
    prepared: Prepared,
    board: &Board,
    chip: &ChipInformation,
) -> Result<(Prepared, Vec<Warning>), Error> {
    let Some(minimum) = prepared
        .layout
        .as_ref()
        .and_then(|layout| layout.minimum_flash_size)
    else {
        return Ok((prepared, Vec::new()));
    };
    let Some(detected) = chip.flash_size else {
        let warning = Warning::FlashSizeNotDetected {
            minimum_flash_size: minimum,
        };
        return Ok((prepared, alloc::vec![warning]));
    };
    if detected < minimum {
        return Err(Error::FlashTooSmall {
            layout: minimum,
            detected,
        });
    }
    if detected == minimum {
        return Ok((prepared, Vec::new()));
    }
    let (prepared, changes) = prepare::place_for_flash(prepared, board, detected)?;
    let mut warnings = Vec::new();
    if !changes.grown.is_empty() {
        warnings.push(Warning::PartitionsGrew {
            flash_size: detected,
            minimum_flash_size: minimum,
            partitions: changes.grown,
            protocol: board.protocol,
        });
    }
    if !changes.moved.is_empty() {
        warnings.push(Warning::PartitionsMoved {
            flash_size: detected,
            minimum_flash_size: minimum,
            partitions: changes.moved,
        });
    }
    Ok((prepared, warnings))
}

/// Checks every image, then every erase entry, against what `connect` reported: each must start
/// at or after `writable_from`, end at or before `writable_to` and inside the `flash_size` bytes
/// of the chip, and when the
/// bootloader erases to the end of flash, every erase entry must end there.
fn check_against_chip(prepared: &Prepared, chip: &ChipInformation) -> Result<(), Error> {
    let images = prepared.images.iter().map(|image| {
        let size = u64::try_from(image.bytes.len()).unwrap_or(u64::MAX);
        ("image", &image.label, image.offset, size)
    });
    let erase = prepared.erase.iter().map(|entry| {
        (
            "erase entry",
            &entry.label,
            entry.region.offset,
            entry.region.size,
        )
    });
    for (kind, label, offset, size) in images.chain(erase) {
        if let Some(writable_from) = chip.writable_from {
            if offset < writable_from {
                return Err(Error::BelowWritableFlash {
                    what: format!("{kind} \"{label}\""),
                    offset,
                    writable_from,
                });
            }
        }
        let end = offset.saturating_add(size);
        if let Some(writable_to) = chip.writable_to {
            if end > writable_to {
                return Err(Error::AboveWritableFlash {
                    what: format!("{kind} \"{label}\""),
                    end,
                    writable_to,
                    protocol: chip.protocol,
                });
            }
        }
        if let Some(flash_size) = chip.flash_size {
            if end > flash_size {
                return Err(Error::OutsideDetectedFlash {
                    what: format!("{kind} \"{label}\""),
                    end,
                    flash_size,
                });
            }
        }
    }
    // Where the bootloader erases from an offset to the end of flash, an erase entry ending
    // earlier would also erase what follows it. `--erase-all` ends there, but runs first: every
    // entry is checked before it. An empty entry erases nothing.
    if let (true, Some(flash_size)) = (chip.erases_to_end, chip.flash_size) {
        for entry in &prepared.erase {
            let end = entry.region.offset.saturating_add(entry.region.size);
            if entry.region.size != 0 && end != flash_size {
                return Err(Error::EraseShortOfFlashEnd {
                    chip: chip.chip.clone(),
                    entry: entry.label.clone(),
                    offset: entry.region.offset,
                    end,
                    flash_size,
                });
            }
        }
    }
    Ok(())
}

/// The warning of a flash whose bootloader erases from an offset to the end of flash: writing
/// erases from the lowest image to the end, unless `--erase-all` or an erase entry from that image
/// or below did already. It names the partitions of the layout in that range that no image is
/// written to.
fn erases_to_end_warning(
    prepared: &Prepared,
    chip: &ChipInformation,
    erase_all: bool,
) -> Option<Warning> {
    let (true, Some(flash_size), false) = (chip.erases_to_end, chip.flash_size, erase_all) else {
        return None;
    };
    let from = prepared
        .images
        .iter()
        .filter(|image| !image.bytes.is_empty())
        .map(|image| image.offset)
        .min()?;
    let erased_before = prepared
        .erase
        .iter()
        .any(|entry| entry.region.size != 0 && entry.region.offset <= from);
    if erased_before {
        return None;
    }
    let written = |name: &str| {
        prepared
            .images
            .iter()
            .any(|image| image.partition.as_deref() == Some(name))
    };
    let partitions = prepared
        .layout
        .iter()
        .flat_map(|layout| &layout.partitions)
        .filter(|partition| {
            partition.size != 0
                && partition.offset < flash_size
                && partition.end() > from
                && !written(&partition.definition.name)
        })
        .map(|partition| partition.definition.name.clone())
        .collect();
    Some(Warning::ErasesToEnd {
        from,
        to: flash_size,
        partitions,
    })
}

/// Where the lowest bootloader partition at or above the application starts, when the layout
/// has both and the bootloader sits at the top of the flash (`writable_to`, AVR): a bootloader
/// the chip does not report as protected (an Optiboot of 512 bytes in a boot section of 2 KiB)
/// still keeps its partition. A bootloader below the application (SAM-BA, Espressif) leaves the
/// end of `--erase-all` to the chip: a partition of the layout that calls itself a bootloader
/// above the application does not shorten it.
fn bootloader_above_application(
    chip: &ChipInformation,
    layout: Option<&ResolvedLayout>,
) -> Option<u64> {
    chip.writable_to?;
    let layout = layout?;
    let application = layout
        .partitions
        .iter()
        .filter(|partition| partition.definition.role == Role::Firmware)
        .map(|partition| partition.offset)
        .min()?;
    layout
        .partitions
        .iter()
        .filter(|partition| {
            partition.definition.role == Role::Bootloader && partition.offset >= application
        })
        .map(|partition| partition.offset)
        .min()
}

/// The region `--erase-all` erases: the flash of the chip, else of the layout, from where the
/// bootloader starts writing to where the bootloader at the top of the flash starts: the
/// smaller of `writable_to` and `bootloader_start`, the bootloader partition of the layout above
/// the application.
fn erase_all_region(
    chip: &ChipInformation,
    layout_size: Option<u64>,
    bootloader_start: Option<u64>,
) -> Result<Region, Error> {
    let mut flash_size = chip
        .flash_size
        .or(layout_size)
        .ok_or(Error::UnknownFlashSize)?;
    for limit in [chip.writable_to, bootloader_start].into_iter().flatten() {
        flash_size = flash_size.min(limit);
    }
    let offset = chip.writable_from.unwrap_or(0);
    let size = flash_size
        .checked_sub(offset)
        .ok_or_else(|| Error::OutsideDetectedFlash {
            what: "bootloader".into(),
            end: offset,
            flash_size,
        })?;
    Ok(Region { offset, size })
}

/// The USB identities the candidate boards show in their bootloader, in manifest order, each once.
fn bootloader_devices(boards: &[&Board]) -> Vec<DeviceFilter> {
    let mut devices: Vec<DeviceFilter> = Vec::new();
    for device in boards.iter().flat_map(|board| &board.bootloader_devices) {
        if !devices.contains(device) {
            devices.push(device.clone());
        }
    }
    devices
}

/// What a stage does with a segment, such as `writing "application" at 0x20000`.
fn doing(stage: &str, segment: &Segment<'_>) -> String {
    format!("{stage} \"{}\" at 0x{:x}", segment.label, segment.offset)
}

/// The part of [`flash`] between connecting and resetting into the application.
async fn flash_connected<P: PortProvider, S: ImageSource>(
    backend: &mut Backends<'_, P>,
    selection: &Selection<'_>,
    prepared: Option<Prepared>,
    chip: ChipInformation,
    hooks: Hooks<'_, S>,
    options: &FlashOptions,
) -> Result<FlashReport, Error> {
    let Hooks {
        source,
        verifier,
        progress,
    } = hooks;
    let board = selection.pick(&chip.chip)?;
    let prepared = match prepared {
        Some(prepared) => prepared,
        None => prepare(board, source, verifier).await?,
    };
    // A `detect` layout follows the flash of the chip; everything below reads the layout and the
    // places resolved for it, so nothing is erased before they are checked too.
    let (mut prepared, resolved) = resolve_for_chip(prepared, board, &chip)?;
    for warning in &resolved {
        progress.message(&warning.to_string());
    }
    prepared.warnings.extend(resolved);
    let layout_size = prepared.layout.as_ref().map(|layout| layout.flash.size);
    if let (Some(layout), Some(detected)) = (layout_size, chip.flash_size) {
        if layout > detected {
            return Err(Error::FlashTooSmall { layout, detected });
        }
    }
    // Nothing is erased before every image and erase entry is known to fit: `prepare` checked
    // the address range of the protocol and the layout, this checks the flash of the chip and
    // the part its bootloader protects, which is all a board without a layout (esp-web-tools)
    // is checked against.
    check_against_chip(&prepared, &chip)?;
    let erases_to_end = erases_to_end_warning(&prepared, &chip, options.erase_all);
    let erase_all = if options.erase_all {
        let bootloader_start = bootloader_above_application(&chip, prepared.layout.as_ref());
        Some(erase_all_region(&chip, layout_size, bootloader_start)?)
    } else {
        None
    };
    let erases: Vec<Region> = prepared.erase.iter().map(|entry| entry.region).collect();
    let mut images: Vec<&PreparedImage> = prepared.images.iter().collect();
    images.sort_by_key(|image| image.offset);
    let segments: Vec<Segment<'_>> = images
        .iter()
        .map(|image| Segment {
            label: &image.label,
            offset: image.offset,
            data: &image.bytes,
        })
        .collect();
    // The backend sees the whole flash, in the order it is sent, before the first erase: a
    // bootloader that erases in its own order (the Mega 2560's) would otherwise refuse an image
    // after the erases and the images before it went through.
    backend.check_plan(erase_all, &erases, &segments)?;
    if let Some(region) = erase_all {
        erase(backend, region, progress).await?;
    }
    for region in &erases {
        erase(backend, *region, progress).await?;
    }
    // One segment at a time, so that a failure names the image.
    for segment in &segments {
        backend
            .write(core::slice::from_ref(segment), progress)
            .await
            .map_err(|error| error.during(|| doing("writing", segment)))?;
    }
    let mut warnings = prepared.warnings;
    // The backend reported the erase as it happened; the report keeps it.
    warnings.extend(erases_to_end);
    // The ROM loader in secure download mode refuses SPI_FLASH_MD5: failing after a successful
    // write would only hide that the write went through.
    let verify = options.verify && !chip.secure_download_mode;
    if options.verify && !verify {
        let warning = Warning::NotVerifiedInSecureDownloadMode;
        progress.message(&warning.to_string());
        warnings.push(warning);
    }
    if verify {
        for segment in &segments {
            backend
                .verify(core::slice::from_ref(segment), progress)
                .await
                .map_err(|error| error.during(|| doing("verifying", segment)))?;
        }
    }
    Ok(FlashReport {
        board: board.identifier.clone(),
        chip,
        images: segments
            .iter()
            .map(|segment| WrittenImage {
                label: segment.label.into(),
                offset: segment.offset,
                size: u64::try_from(segment.data.len()).unwrap_or(u64::MAX),
            })
            .collect(),
        verified: verify,
        warnings,
    })
}

/// Connects to the device on `port` and tells what chip it is, then resets it into its
/// application. The baud rate the bootloader starts at is kept: nothing is transferred.
///
/// The protocol is `protocol`, else the one the USB vendor of the port speaks.
pub async fn detect<P: PortProvider>(
    port: P::Port,
    provider: &mut P,
    protocol: Option<Protocol>,
    progress: &mut dyn Progress,
) -> Result<ChipInformation, Error> {
    let identity = port.identity();
    let protocol = match protocol.or_else(|| identity.as_ref().and_then(protocol_for_identity)) {
        Some(protocol) => protocol,
        None => return Err(Error::ProtocolRequired { identity }),
    };
    let mut backend = Backends::new(protocol, port, provider, None, &BackendOptions::default())?;
    report(progress, Stage::Connecting, 0, 0, 0);
    let chip = backend
        .identify(progress)
        .await
        .map_err(|error| error.during(|| "connecting".into()))?;
    reset(&mut backend, progress).await?;
    Ok(chip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorClass;
    use crate::transport::DeviceFilter;

    fn chip(flash_size: Option<u64>, writable_from: Option<u64>) -> ChipInformation {
        ChipInformation {
            protocol: Protocol::Samba,
            chip: "nrf52840".into(),
            flash_size,
            secure_download_mode: false,
            usb: None,
            writable_from,
            writable_to: None,
            erases_to_end: false,
        }
    }

    /// Images as (label, offset, size) and erase entries as (label, offset, size).
    fn prepared(images: &[(&str, u64, usize)], erase: &[(&str, u64, u64)]) -> Prepared {
        Prepared {
            layout: None,
            definition: None,
            images: images
                .iter()
                .map(|&(label, offset, size)| PreparedImage {
                    label: label.into(),
                    partition: None,
                    offset,
                    bytes: alloc::vec![0xA5; size],
                    placement: ImagePlacement::Offset,
                })
                .collect(),
            erase: erase
                .iter()
                .map(|&(label, offset, size)| PreparedErase {
                    label: label.into(),
                    partition: None,
                    region: Region { offset, size },
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn images_below_writable_flash_are_refused() {
        let protected = chip(Some(0x10_0000), Some(0x1_0000));
        let error =
            check_against_chip(&prepared(&[("bootloader", 0, 16)], &[]), &protected).unwrap_err();
        assert_eq!(
            error,
            Error::BelowWritableFlash {
                what: "image \"bootloader\"".into(),
                offset: 0,
                writable_from: 0x1_0000,
            }
        );
        assert_eq!(error.class(), ErrorClass::Input);
        let message = error.to_string();
        assert!(message.contains("0x10000"), "{message}");
        assert!(message.contains("\"bootloader\""), "{message}");
        assert_eq!(
            check_against_chip(
                &prepared(&[], &[("region at 0x8000", 0x8000, 0x1_0000)]),
                &protected
            ),
            Err(Error::BelowWritableFlash {
                what: "erase entry \"region at 0x8000\"".into(),
                offset: 0x8000,
                writable_from: 0x1_0000,
            })
        );
        // Images come before erase entries, whatever their offsets.
        assert_eq!(
            check_against_chip(
                &prepared(
                    &[("bootloader", 0x4000, 16)],
                    &[("region at 0x0", 0, 0x1000)]
                ),
                &protected
            ),
            Err(Error::BelowWritableFlash {
                what: "image \"bootloader\"".into(),
                offset: 0x4000,
                writable_from: 0x1_0000,
            })
        );
        assert_eq!(
            check_against_chip(&prepared(&[("application", 0x1_0000, 16)], &[]), &protected),
            Ok(())
        );
        assert_eq!(
            check_against_chip(
                &prepared(&[("bootloader", 0, 16)], &[]),
                &chip(Some(0x10_0000), None)
            ),
            Ok(())
        );
        // Neither size nor start known: nothing to check against.
        assert_eq!(
            check_against_chip(&prepared(&[("bootloader", 0, 16)], &[]), &chip(None, None)),
            Ok(())
        );
    }

    #[test]
    fn below_writable_flash_comes_before_outside_detected_flash() {
        let protected = chip(Some(0x4_0000), Some(0x2000));
        assert!(matches!(
            check_against_chip(&prepared(&[("both", 0x1000, 0x5_0000)], &[]), &protected),
            Err(Error::BelowWritableFlash { .. })
        ));
    }

    #[test]
    fn image_ending_at_the_end_of_flash() {
        let samd21 = chip(Some(0x4_0000), Some(0x2000));
        assert_eq!(
            check_against_chip(
                &prepared(&[("application", 0x2000, 0x3_E000)], &[]),
                &samd21
            ),
            Ok(())
        );
        assert_eq!(
            check_against_chip(
                &prepared(&[("application", 0x2000, 0x3_E001)], &[]),
                &samd21
            ),
            Err(Error::OutsideDetectedFlash {
                what: "image \"application\"".into(),
                end: 0x4_0001,
                flash_size: 0x4_0000,
            })
        );
        assert_eq!(
            check_against_chip(&prepared(&[], &[("storage", 0x3_F000, 0x1000)]), &samd21),
            Ok(())
        );
        assert_eq!(
            check_against_chip(&prepared(&[], &[("storage", 0x3_F000, 0x1001)]), &samd21),
            Err(Error::OutsideDetectedFlash {
                what: "erase entry \"storage\"".into(),
                end: 0x4_0001,
                flash_size: 0x4_0000,
            })
        );
    }

    #[test]
    fn erase_entries_end_at_the_end_of_flash_where_erases_do() {
        let samd21 = ChipInformation {
            chip: "samd21g18a".into(),
            erases_to_end: true,
            ..chip(Some(0x4_0000), Some(0x2000))
        };
        let to_the_end = ("to the end", 0x1_0000, 0x3_0000);
        let short = ("short", 0x8000, 0x1000);
        assert_eq!(
            check_against_chip(&prepared(&[], &[to_the_end]), &samd21),
            Ok(())
        );
        assert_eq!(
            check_against_chip(&prepared(&[], &[to_the_end, short]), &samd21),
            Err(Error::EraseShortOfFlashEnd {
                chip: "samd21g18a".into(),
                entry: "short".into(),
                offset: 0x8000,
                end: 0x9000,
                flash_size: 0x4_0000,
            })
        );
        // An empty entry erases nothing, wherever it lies.
        assert_eq!(
            check_against_chip(&prepared(&[], &[("empty", 0x8000, 0)]), &samd21),
            Ok(())
        );
        // Where erases stop at the end of the region, a short entry is fine.
        let region_erases = chip(Some(0x4_0000), Some(0x2000));
        assert_eq!(
            check_against_chip(&prepared(&[], &[short]), &region_erases),
            Ok(())
        );
    }

    #[test]
    fn writes_that_erase_to_the_end_of_flash_are_warned_about() {
        let samd21 = ChipInformation {
            chip: "samd21g18a".into(),
            erases_to_end: true,
            ..chip(Some(0x4_0000), Some(0x2000))
        };
        let layout = crate::layout::Layout::from_toml(
            "[flash]\nsize = \"256 KiB\"\n\n\
             [[partition]]\nname = \"bootloader\"\nrole = \"bootloader\"\noffset = 0x0\nsize = 0x2000\n\n\
             [[partition]]\nname = \"application\"\nrole = \"firmware\"\nsize = 0x3_0000\n\n\
             [[partition]]\nname = \"configuration\"\nrole = \"custom\"\nsize = 0x4000\n\n\
             [[partition]]\nname = \"storage\"\nrole = \"storage\"\nsize = \"remaining\"\n",
        )
        .unwrap()
        .resolve()
        .unwrap();
        let mut plan = prepared(&[("application", 0x2000, 16)], &[]);
        plan.layout = Some(layout);
        plan.images[0].partition = Some("application".into());
        let warning = erases_to_end_warning(&plan, &samd21, false);
        assert_eq!(
            warning,
            Some(Warning::ErasesToEnd {
                from: 0x2000,
                to: 0x4_0000,
                partitions: alloc::vec!["configuration".into(), "storage".into()],
            })
        );
        assert_eq!(
            warning.unwrap().to_string(),
            "the bootloader erased the flash from 0x2000 to its end at 0x40000 before writing, so what was stored there besides the images is gone, including the partitions \"configuration\", \"storage\""
        );
        // From the lowest image that has bytes; the partitions an image is written to are not
        // named.
        let mut two = prepared(&[("empty", 0x2000, 0), ("application", 0x3_2000, 16)], &[]);
        two.layout = plan.layout.clone();
        two.images[1].partition = Some("configuration".into());
        assert_eq!(
            erases_to_end_warning(&two, &samd21, false),
            Some(Warning::ErasesToEnd {
                from: 0x3_2000,
                to: 0x4_0000,
                partitions: alloc::vec!["storage".into()],
            })
        );
        // Without a layout, nothing to name.
        let bare = prepared(&[("image at 0x2000", 0x2000, 16)], &[]);
        assert_eq!(
            erases_to_end_warning(&bare, &samd21, false),
            Some(Warning::ErasesToEnd {
                from: 0x2000,
                to: 0x4_0000,
                partitions: Vec::new(),
            })
        );
        assert_eq!(
            Warning::ErasesToEnd {
                from: 0x2000,
                to: 0x4_0000,
                partitions: Vec::new(),
            }
            .to_string(),
            "the bootloader erased the flash from 0x2000 to its end at 0x40000 before writing, so what was stored there besides the images is gone"
        );
        // Asked for: `--erase-all`, or an erase entry from the lowest image or below.
        assert_eq!(erases_to_end_warning(&bare, &samd21, true), None);
        let erased = prepared(
            &[("image at 0x2000", 0x2000, 16)],
            &[("region at 0x2000", 0x2000, 0x3_E000)],
        );
        assert_eq!(erases_to_end_warning(&erased, &samd21, false), None);
        // An erase entry above the image, or an empty one, does not erase from the image.
        let above = prepared(
            &[("image at 0x2000", 0x2000, 16)],
            &[
                ("region at 0x10000", 0x1_0000, 0x3_0000),
                ("empty", 0x2000, 0),
            ],
        );
        assert!(erases_to_end_warning(&above, &samd21, false).is_some());
        // Nothing written, or a bootloader that erases only what it writes.
        assert_eq!(
            erases_to_end_warning(&prepared(&[("empty", 0x2000, 0)], &[]), &samd21, false),
            None
        );
        assert_eq!(
            erases_to_end_warning(&bare, &chip(Some(0x4_0000), Some(0x2000)), false),
            None
        );
    }

    #[test]
    fn erase_all_starts_at_the_writable_flash() {
        assert_eq!(
            erase_all_region(&chip(Some(0x10_0000), Some(0x1_0000)), None, None),
            Ok(Region {
                offset: 0x1_0000,
                size: 0xF_0000,
            })
        );
        assert_eq!(
            erase_all_region(&chip(Some(0x80_0000), None), None, None),
            Ok(Region {
                offset: 0,
                size: 0x80_0000,
            })
        );
        assert_eq!(
            erase_all_region(&chip(None, None), Some(0x4_0000), None),
            Ok(Region {
                offset: 0,
                size: 0x4_0000,
            })
        );
        assert_eq!(
            erase_all_region(&chip(None, None), None, None),
            Err(Error::UnknownFlashSize)
        );
        // The chip's size wins over the layout's.
        assert_eq!(
            erase_all_region(&chip(Some(0x10_0000), Some(0x1_0000)), Some(0x8_0000), None),
            Ok(Region {
                offset: 0x1_0000,
                size: 0xF_0000,
            })
        );
    }

    fn avr(flash_size: u64, writable_to: u64) -> ChipInformation {
        ChipInformation {
            protocol: Protocol::Stk500v1,
            chip: "atmega328p".into(),
            writable_to: Some(writable_to),
            ..chip(Some(flash_size), None)
        }
    }

    #[test]
    fn images_above_writable_flash_are_refused() {
        let uno = avr(0x8000, 0x7E00);
        let error =
            check_against_chip(&prepared(&[("application", 0, 0x7E01)], &[]), &uno).unwrap_err();
        assert_eq!(
            error,
            Error::AboveWritableFlash {
                what: "image \"application\"".into(),
                end: 0x7E01,
                writable_to: 0x7E00,
                protocol: Protocol::Stk500v1,
            }
        );
        assert_eq!(error.class(), ErrorClass::Input);
        assert_eq!(
            check_against_chip(&prepared(&[("application", 0, 0x7E00)], &[]), &uno),
            Ok(())
        );
        assert_eq!(
            check_against_chip(&prepared(&[], &[("bootloader", 0x7E00, 0x200)]), &uno),
            Err(Error::AboveWritableFlash {
                what: "erase entry \"bootloader\"".into(),
                end: 0x8000,
                writable_to: 0x7E00,
                protocol: Protocol::Stk500v1,
            })
        );
        // Without `writable_to`, the flash is writable up to its size.
        assert_eq!(
            check_against_chip(
                &prepared(&[("application", 0, 0x8000)], &[]),
                &chip(Some(0x8000), None)
            ),
            Ok(())
        );
        // Past the bootloader, but also past the flash: the bootloader is named first.
        assert!(matches!(
            check_against_chip(&prepared(&[("application", 0, 0x8001)], &[]), &uno),
            Err(Error::AboveWritableFlash { .. })
        ));
    }

    #[test]
    fn erase_all_stops_at_the_bootloader() {
        assert_eq!(
            erase_all_region(&avr(0x8000, 0x7E00), None, None),
            Ok(Region {
                offset: 0,
                size: 0x7E00,
            })
        );
        // The Mega 2560: its bootloader erases nothing from 0x3C000, twice its size under the
        // end of flash.
        assert_eq!(
            erase_all_region(&avr(0x4_0000, 0x3_C000), None, None),
            Ok(Region {
                offset: 0,
                size: 0x3_C000,
            })
        );
    }

    #[test]
    fn erase_all_stops_at_the_bootloader_of_the_layout() {
        // A Nano: Optiboot reports 0x7E00, but the layout keeps 2 KiB for the bootloader.
        let layout =
            crate::layout::Layout::from_toml(include_str!("../../tests/layouts/nano.toml"))
                .unwrap()
                .resolve()
                .unwrap();
        let nano = avr(0x8000, 0x7E00);
        let start = bootloader_above_application(&nano, Some(&layout));
        assert_eq!(start, Some(0x7800));
        assert_eq!(
            erase_all_region(&nano, Some(0x8000), start),
            Ok(Region {
                offset: 0,
                size: 0x7800,
            })
        );
        // Without a layout, the chip's limit stands.
        assert_eq!(bootloader_above_application(&nano, None), None);
        // A bootloader below the application (SAMD21, nRF52) is not at the top.
        let below = crate::layout::Layout::from_toml(
            "[flash]\nsize = \"256 KiB\"\n\n\
             [[partition]]\nname = \"bootloader\"\nrole = \"bootloader\"\noffset = 0x0\nsize = 0x2000\n\n\
             [[partition]]\nname = \"application\"\nrole = \"firmware\"\nsize = 0x3_E000\n",
        )
        .unwrap()
        .resolve()
        .unwrap();
        assert_eq!(bootloader_above_application(&nano, Some(&below)), None);
    }

    #[test]
    fn erase_all_of_a_bootloader_below_the_application_ignores_the_layout() {
        // A SAMD21 layout that names a partition above the application a bootloader: the
        // SAM-BA bootloader sits below 0x2000 and reports no `writable_to`, so --erase-all still
        // reaches the end of flash.
        let layout = crate::layout::Layout::from_toml(
            "[flash]\nsize = \"256 KiB\"\n\n\
             [[partition]]\nname = \"bootloader\"\nrole = \"bootloader\"\noffset = 0x0\nsize = 0x2000\n\n\
             [[partition]]\nname = \"application\"\nrole = \"firmware\"\nsize = 0x3_C000\n\n\
             [[partition]]\nname = \"second-stage\"\nrole = \"bootloader\"\nsize = 0x2000\n",
        )
        .unwrap()
        .resolve()
        .unwrap();
        let samd21 = ChipInformation {
            chip: "samd21g18a".into(),
            erases_to_end: true,
            ..chip(Some(0x4_0000), Some(0x2000))
        };
        let start = bootloader_above_application(&samd21, Some(&layout));
        assert_eq!(start, None);
        assert_eq!(
            erase_all_region(&samd21, Some(0x4_0000), start),
            Ok(Region {
                offset: 0x2000,
                size: 0x3_E000,
            })
        );
        // The same layout on a chip whose bootloader sits at the top does stop there.
        assert_eq!(
            bootloader_above_application(&avr(0x4_0000, 0x3_C000), Some(&layout)),
            Some(0x3_E000)
        );
    }

    #[test]
    fn erase_all_of_a_bootloader_larger_than_the_flash() {
        assert_eq!(
            erase_all_region(&chip(Some(0x1000), Some(0x2000)), None, None),
            Err(Error::OutsideDetectedFlash {
                what: "bootloader".into(),
                end: 0x2000,
                flash_size: 0x1000,
            })
        );
    }

    /// A board of `protocol` and the layout `text` resolved for its minimum, erasing the
    /// partitions `erase` by name.
    fn layout_prepared(protocol: &str, text: &str, erase: &[&str]) -> (Board, Prepared) {
        let mut manifest = format!(
            "[[board]]\nidentifier = \"b\"\nprotocol = \"{protocol}\"\nchip = \"c\"\nlayout = \"layout.toml\"\n"
        );
        for name in erase {
            manifest.push_str(&format!("[[board.erase]]\npartition = \"{name}\"\n"));
        }
        let board = Manifest::from_toml(&manifest).unwrap().boards.remove(0);
        let definition = crate::layout::Layout::from_toml(text).unwrap();
        let layout = definition.resolve().unwrap();
        let erase = erase
            .iter()
            .map(|name| {
                let partition = layout.partition(name).unwrap();
                PreparedErase {
                    label: (*name).into(),
                    partition: Some((*name).into()),
                    region: Region {
                        offset: partition.offset,
                        size: partition.size,
                    },
                }
            })
            .collect();
        let prepared = Prepared {
            layout: Some(layout),
            definition: Some(definition),
            images: Vec::new(),
            erase,
            warnings: Vec::new(),
        };
        (board, prepared)
    }

    fn espressif(flash_size: Option<u64>) -> ChipInformation {
        ChipInformation {
            protocol: Protocol::Espressif,
            chip: "esp32-s3".into(),
            ..chip(flash_size, None)
        }
    }

    const DETECT: &str = include_str!("../../tests/layouts/esp32-s3-detect.toml");

    #[test]
    fn resolve_for_chip_follows_the_reported_flash() {
        let fixed = include_str!("../../tests/layouts/esp32-s3-8mb.toml");
        let (board, prepared) = layout_prepared("espressif", fixed, &["storage"]);
        for size in [None, Some(0x80_0000), Some(0x100_0000)] {
            let (same, warnings) =
                resolve_for_chip(prepared.clone(), &board, &espressif(size)).unwrap();
            assert_eq!(same, prepared);
            assert!(warnings.is_empty());
        }

        let (board, prepared) = layout_prepared("espressif", DETECT, &["storage"]);
        let (same, warnings) =
            resolve_for_chip(prepared.clone(), &board, &espressif(None)).unwrap();
        assert_eq!(same, prepared);
        assert_eq!(
            warnings,
            [Warning::FlashSizeNotDetected {
                minimum_flash_size: 0x40_0000
            }]
        );
        assert_eq!(
            warnings[0].to_string(),
            "the chip did not report its flash size, so the layout was resolved for its minimum of 0x400000 bytes: the partitions that grow with the flash have their smallest size"
        );
        assert_eq!(
            resolve_for_chip(prepared.clone(), &board, &espressif(Some(0x20_0000))).unwrap_err(),
            Error::FlashTooSmall {
                layout: 0x40_0000,
                detected: 0x20_0000
            }
        );
        let (same, warnings) =
            resolve_for_chip(prepared.clone(), &board, &espressif(Some(0x40_0000))).unwrap();
        assert_eq!(same, prepared);
        assert!(warnings.is_empty());

        let (grown, warnings) =
            resolve_for_chip(prepared, &board, &espressif(Some(0x80_0000))).unwrap();
        let storage = crate::layout::GrownPartition {
            name: "storage".into(),
            size_at_minimum: 0xE_0000,
            size: 0x4E_0000,
        };
        assert_eq!(
            warnings,
            [Warning::PartitionsGrew {
                flash_size: 0x80_0000,
                minimum_flash_size: 0x40_0000,
                partitions: alloc::vec![storage],
                protocol: Protocol::Espressif,
            }]
        );
        assert_eq!(
            warnings[0].to_string(),
            "the chip has 0x800000 bytes of flash, more than the minimum of 0x400000 of the layout, so partitions grew: \"storage\" from 0xe0000 to 0x4e0000 bytes; the bootloader image must be built for this flash size or with flash size detection"
        );
        assert_eq!(grown.layout.as_ref().unwrap().flash.size, 0x80_0000);
        assert_eq!(
            grown.erase[0].region,
            Region {
                offset: 0x32_0000,
                size: 0x4E_0000
            }
        );
    }

    #[test]
    fn resolve_for_chip_tells_what_moved() {
        let layout = format!(
            "{DETECT}\n[[partition]]\nname = \"coredump\"\nrole = \"crash-record\"\nsize = \"64 KiB\"\n"
        );
        let (board, prepared) = layout_prepared("samba", &layout, &[]);
        let samba = ChipInformation {
            chip: "samd21g18a".into(),
            ..chip(Some(0x100_0000), None)
        };
        let (_, warnings) = resolve_for_chip(prepared, &board, &samba).unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(matches!(&warnings[0], Warning::PartitionsGrew { .. }));
        // Only the Espressif bootloader reads a flash size of its own.
        assert_eq!(
            warnings[0].to_string(),
            "the chip has 0x1000000 bytes of flash, more than the minimum of 0x400000 of the layout, so partitions grew: \"storage\" from 0xd0000 to 0xcd0000 bytes"
        );
        assert_eq!(
            warnings[1],
            Warning::PartitionsMoved {
                flash_size: 0x100_0000,
                minimum_flash_size: 0x40_0000,
                partitions: alloc::vec![crate::layout::MovedPartition {
                    name: "coredump".into(),
                    offset_at_minimum: 0x3F_0000,
                    offset: 0xFF_0000,
                }],
            }
        );
        assert_eq!(
            warnings[1].to_string(),
            "on the 0x1000000 bytes of flash of the chip, partitions start elsewhere than on the minimum of 0x400000: \"coredump\" at 0xff0000 instead of 0x3f0000; data stored there by a firmware for another flash size is not where the layout now puts it"
        );
    }

    #[test]
    fn resolve_for_chip_then_checks_what_the_bootloader_keeps() {
        // AVR: the flash the chip reports includes the bootloader at its top, so a partition
        // that grows to the end of the flash covers it. Erasing it is refused before any erase.
        let avr_layout = "[flash]\nsize = \"detect\"\nminimum = \"16 KiB\"\nalignment = 128\n\n\
             [[partition]]\nname = \"application\"\nrole = \"firmware\"\noffset = 0x0\nsize = \"8 KiB\"\n\n\
             [[partition]]\nname = \"storage\"\nrole = \"storage\"\nsize = \"remaining\"\n";
        let (board, prepared) = layout_prepared("stk500v1", avr_layout, &["storage"]);
        let uno = avr(0x8000, 0x7E00);
        let (resolved, warnings) = resolve_for_chip(prepared, &board, &uno).unwrap();
        assert!(matches!(&warnings[..], [Warning::PartitionsGrew { .. }]));
        assert_eq!(
            check_against_chip(&resolved, &uno),
            Err(Error::AboveWritableFlash {
                what: "erase entry \"storage\"".into(),
                end: 0x8000,
                writable_to: 0x7E00,
                protocol: Protocol::Stk500v1,
            })
        );

        // SAMD21: erases end at the end of flash, which a capped partition no longer reaches on
        // a larger chip.
        let samd21_layout = "[flash]\nsize = \"detect\"\nminimum = \"128 KiB\"\n\n\
             [[partition]]\nname = \"bootloader\"\nrole = \"bootloader\"\noffset = 0x0\nsize = 0x2000\n\n\
             [[partition]]\nname = \"application\"\nrole = \"firmware\"\nsize = 0x1_0000\n\n\
             [[partition]]\nname = \"storage\"\nrole = \"storage\"\nsize = { minimum = \"32 KiB\", maximum = \"64 KiB\" }\n";
        let (board, prepared) = layout_prepared("samba", samd21_layout, &["storage"]);
        let samd21 = |flash_size| ChipInformation {
            chip: "samd21g18a".into(),
            erases_to_end: true,
            ..chip(Some(flash_size), Some(0x2000))
        };
        let (resolved, _) = resolve_for_chip(prepared.clone(), &board, &samd21(0x2_0000)).unwrap();
        assert_eq!(check_against_chip(&resolved, &samd21(0x2_0000)), Ok(()));
        let (resolved, _) = resolve_for_chip(prepared, &board, &samd21(0x4_0000)).unwrap();
        assert_eq!(
            check_against_chip(&resolved, &samd21(0x4_0000)),
            Err(Error::EraseShortOfFlashEnd {
                chip: "samd21g18a".into(),
                entry: "storage".into(),
                offset: 0x1_2000,
                end: 0x2_2000,
                flash_size: 0x4_0000,
            })
        );
    }

    #[test]
    fn bootloader_devices_of_the_candidates() {
        let manifest = Manifest::from_toml(
            "[[board]]\nidentifier = \"first\"\nprotocol = \"samba\"\nchip = \"nrf52840\"\n\
             bootloader-devices = [{ vendor = 0x2341, product = 0x005a }]\n\
             [[board.image]]\noffset = 0x10000\npath = \"first.bin\"\n\
             [[board]]\nidentifier = \"second\"\nprotocol = \"samba\"\nchip = \"nrf52840\"\n\
             bootloader-devices = [{ vendor = 0x2341, product = 0x005a }, { vendor = 0x2341, product = 0x0054 }]\n\
             [[board.image]]\noffset = 0x10000\npath = \"second.bin\"\n",
        )
        .unwrap();
        let boards: Vec<&crate::manifest::Board> = manifest.boards.iter().collect();
        let filter = |product| DeviceFilter {
            vendor: 0x2341,
            product: Some(product),
            serial_number: None,
        };
        assert_eq!(
            bootloader_devices(&boards),
            [filter(0x005a), filter(0x0054)]
        );
        assert_eq!(bootloader_devices(&[]), []);
    }
}
