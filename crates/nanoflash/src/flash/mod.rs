//! Flashing a board from its manifest and detecting a chip, through one code path for every
//! backend: choosing the board a device is, preparing its images, then connecting, erasing,
//! writing, verifying and resetting.

mod prepare;
mod select;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use serde::Serialize;

pub use crate::backend::BackendOptions;
use crate::backend::{
    Backend, Backends, ChipInformation, Progress, ProgressEvent, Region, Segment, Stage,
};
use crate::error::Error;
use crate::manifest::{Manifest, Protocol};
use crate::transport::{PortProvider, SerialPort};
pub use prepare::{Prepared, PreparedErase, PreparedImage, Warning, image_build, prepare};
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
    /// flash size of the layout.
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
    /// Whether what was written was checked.
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
    backend.erase(region).await.map_err(|error| {
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
    let mut backend = Backends::new(
        selection.protocol,
        port,
        provider,
        selection.reset.clone(),
        &options.backend,
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

/// Checks that every image and erase entry ends inside the `flash_size` bytes the chip reports.
fn check_detected_flash(prepared: &Prepared, flash_size: u64) -> Result<(), Error> {
    let images = prepared.images.iter().map(|image| {
        let size = u64::try_from(image.bytes.len()).unwrap_or(u64::MAX);
        ("image", &image.label, image.offset.saturating_add(size))
    });
    let erase = prepared.erase.iter().map(|entry| {
        (
            "erase entry",
            &entry.label,
            entry.region.offset.saturating_add(entry.region.size),
        )
    });
    for (kind, label, end) in images.chain(erase) {
        if end > flash_size {
            return Err(Error::OutsideDetectedFlash {
                what: format!("{kind} \"{label}\""),
                end,
                flash_size,
            });
        }
    }
    Ok(())
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
    let layout_size = prepared.layout.as_ref().map(|layout| layout.flash.size);
    if let (Some(layout), Some(detected)) = (layout_size, chip.flash_size) {
        if layout > detected {
            return Err(Error::FlashTooSmall { layout, detected });
        }
    }
    // Nothing is erased before every image and erase entry is known to fit: `prepare` checked
    // the address range of the protocol and the layout, this checks the flash of the chip, which
    // is all a board without a layout (esp-web-tools) is checked against.
    if let Some(detected) = chip.flash_size {
        check_detected_flash(&prepared, detected)?;
    }
    if options.erase_all {
        let size = chip
            .flash_size
            .or(layout_size)
            .ok_or(Error::UnknownFlashSize)?;
        erase(backend, Region { offset: 0, size }, progress).await?;
    }
    for entry in &prepared.erase {
        erase(backend, entry.region, progress).await?;
    }
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
    // One segment at a time, so that a failure names the image.
    for segment in &segments {
        backend
            .write(core::slice::from_ref(segment), progress)
            .await
            .map_err(|error| error.during(|| doing("writing", segment)))?;
    }
    if options.verify {
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
        verified: options.verify,
        warnings: prepared.warnings,
    })
}

/// Connects to the device on `port` and tells what chip it is, then resets it into its
/// application.
///
/// The protocol is `protocol`, else the one the USB vendor of the port speaks.
pub async fn detect<P: PortProvider>(
    port: P::Port,
    provider: &mut P,
    protocol: Option<Protocol>,
    options: &BackendOptions,
    progress: &mut dyn Progress,
) -> Result<ChipInformation, Error> {
    let identity = port.identity();
    let protocol = match protocol.or_else(|| identity.as_ref().and_then(protocol_for_identity)) {
        Some(protocol) => protocol,
        None => return Err(Error::ProtocolRequired { identity }),
    };
    let mut backend = Backends::new(protocol, port, provider, None, options)?;
    let chip = connect(&mut backend, progress).await?;
    reset(&mut backend, progress).await?;
    Ok(chip)
}
