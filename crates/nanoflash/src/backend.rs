//! What every flashing backend offers: connect, erase, write, verify and reset, with progress.

use alloc::string::String;
use core::convert::Infallible;
use core::marker::PhantomData;

use serde::Serialize;

use crate::error::Error;
#[cfg(feature = "espressif")]
use crate::espressif::{EspressifBackend, EspressifOptions};
use crate::manifest::{Protocol, ResetSequence};
use crate::transport::{PortProvider, UsbIdentity};

/// One image to write: its name for messages, where it goes and its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    /// The name of the image, for messages and progress.
    pub label: &'a str,
    /// The flash offset of its first byte.
    pub offset: u64,
    /// Its bytes.
    pub data: &'a [u8],
}

/// A range of flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// The flash offset of the first byte.
    pub offset: u64,
    /// The number of bytes.
    pub size: u64,
}

/// What a backend is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Resetting into the bootloader and detecting the chip.
    Connecting,
    /// Erasing flash.
    Erasing,
    /// Writing images.
    Writing,
    /// Checking what was written.
    Verifying,
    /// Resetting into the application.
    Resetting,
}

/// How far a stage has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProgressEvent {
    /// The stage.
    pub stage: Stage,
    /// The flash offset the stage works on.
    pub offset: u64,
    /// The bytes done so far.
    pub done: u64,
    /// The bytes the stage handles in all.
    pub total: u64,
}

/// Receives progress events and messages from a backend.
pub trait Progress {
    /// A stage moved on.
    fn report(&mut self, event: ProgressEvent);

    /// Something the user may want to know, such as a setting the backend could not apply.
    fn message(&mut self, text: &str) {
        let _ = text;
    }
}

impl Progress for () {
    fn report(&mut self, _event: ProgressEvent) {}
}

impl<F: FnMut(ProgressEvent)> Progress for F {
    fn report(&mut self, event: ProgressEvent) {
        self(event);
    }
}

/// What `connect` found out about the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChipInformation {
    /// The protocol the backend speaks.
    pub protocol: Protocol,
    /// The chip, such as `esp32-s3`.
    pub chip: String,
    /// The size of the flash in bytes, when the backend could read it.
    pub flash_size: Option<u64>,
    /// Whether the chip only accepts the commands of the secure download mode.
    pub secure_download_mode: bool,
    /// The USB identity of the port, when it has one.
    pub usb: Option<UsbIdentity>,
}

/// A flashing backend.
// Futures are not Send: browser APIs are single-threaded (docs/design.md, Constraints).
#[allow(async_fn_in_trait)]
pub trait Backend {
    /// Resets the board into its bootloader and detects the chip.
    async fn connect(&mut self) -> Result<ChipInformation, Error>;

    /// Erases `region`.
    async fn erase(&mut self, region: Region) -> Result<(), Error>;

    /// Writes `segments`, in order.
    async fn write(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error>;

    /// Checks that the flash holds `segments`.
    async fn verify(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error>;

    /// Leaves the bootloader and starts the application.
    async fn reset_to_application(&mut self) -> Result<(), Error>;
}

/// How every backend connects.
#[derive(Debug, Clone, Default)]
pub struct BackendOptions {
    /// The baud rate to switch to after connecting; `None` lets the backend choose (460800
    /// through an Espressif UART bridge).
    pub baud_rate: Option<u32>,
}

/// The backends compiled in, chosen by protocol.
pub enum Backends<'p, P: PortProvider> {
    /// The Espressif ROM loader.
    #[cfg(feature = "espressif")]
    Espressif(EspressifBackend<'p, P>),
    /// No backend: keeps the type parameters used when every backend is left out.
    #[doc(hidden)]
    Unavailable(Infallible, PhantomData<&'p mut P>),
}

impl<'p, P: PortProvider> Backends<'p, P> {
    /// The backend of `protocol` over `port`, which `provider` can reopen; `reset` is the reset
    /// sequence of the board, or `None` to let the backend choose from the port.
    pub fn new(
        protocol: Protocol,
        port: P::Port,
        provider: &'p mut P,
        reset: Option<ResetSequence>,
        options: &BackendOptions,
    ) -> Result<Self, Error> {
        match protocol {
            #[cfg(feature = "espressif")]
            Protocol::Espressif => {
                let options = EspressifOptions {
                    baud_rate: options.baud_rate,
                    ..EspressifOptions::default()
                };
                EspressifBackend::new(port, provider, reset, options).map(Backends::Espressif)
            }
            protocol => {
                let _ = (port, provider, reset, options);
                Err(Error::ProtocolNotEnabled { protocol })
            }
        }
    }

    /// Resets the board into its bootloader and detects the chip, reporting through `progress`
    /// the settings the backend could not apply.
    pub async fn connect(&mut self, progress: &mut dyn Progress) -> Result<ChipInformation, Error> {
        match self {
            #[cfg(feature = "espressif")]
            Backends::Espressif(backend) => backend.connect(progress).await,
            Backends::Unavailable(never, _) => {
                let _ = progress;
                match *never {}
            }
        }
    }
}

impl<P: PortProvider> Backend for Backends<'_, P> {
    async fn connect(&mut self) -> Result<ChipInformation, Error> {
        Backends::connect(self, &mut ()).await
    }

    async fn erase(&mut self, region: Region) -> Result<(), Error> {
        match self {
            #[cfg(feature = "espressif")]
            Backends::Espressif(backend) => backend.erase(region).await,
            Backends::Unavailable(never, _) => {
                let _ = region;
                match *never {}
            }
        }
    }

    async fn write(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        match self {
            #[cfg(feature = "espressif")]
            Backends::Espressif(backend) => backend.write(segments, progress).await,
            Backends::Unavailable(never, _) => {
                let _ = (segments, progress);
                match *never {}
            }
        }
    }

    async fn verify(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        match self {
            #[cfg(feature = "espressif")]
            Backends::Espressif(backend) => backend.verify(segments, progress).await,
            Backends::Unavailable(never, _) => {
                let _ = (segments, progress);
                match *never {}
            }
        }
    }

    async fn reset_to_application(&mut self) -> Result<(), Error> {
        match self {
            #[cfg(feature = "espressif")]
            Backends::Espressif(backend) => backend.reset_to_application().await,
            Backends::Unavailable(never, _) => match *never {},
        }
    }
}
