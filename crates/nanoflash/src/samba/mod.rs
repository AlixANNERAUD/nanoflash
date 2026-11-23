//! The SAM-BA backend: Arduino's SAM-BA bootloaders of the Nano 33 BLE (nRF52840) and of the
//! SAMD21 boards, over an ASCII monitor protocol.

pub(crate) mod command;
pub(crate) mod family;
mod write;

pub use command::crc16;

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;

use crate::backend::{ChipInformation, Progress};
use crate::error::Error;
use crate::manifest::{Protocol, ResetSequence};
use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};
use command::Capabilities;
use family::Family;

/// The USB vendor identifier of Arduino.
const ARDUINO_VENDOR: u16 = 0x2341;
/// Arduino's product identifiers set this bit while a sketch runs, and clear it in the
/// bootloader (`805a` for the Nano 33 BLE's sketch, `005a` for its bootloader).
const SKETCH_PRODUCT_BIT: u16 = 0x8000;
/// The baud rate the sketch reads as a request to start the bootloader.
const TOUCH_BAUD_RATE: u32 = 1200;
/// The baud rate the bootloader's port is opened at; USB ignores it.
const BOOTLOADER_BAUD_RATE: u32 = 115_200;
/// How long the bootloader's USB device may take to appear after the touch.
const REOPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Attempts at `N#` and `V#` before the port is declared silent.
const ATTEMPTS: u8 = 3;
const READ_CHUNK: usize = 64;
/// How long the line must stay quiet before a command, and how long `N#` is waited for.
const QUIET_TIMEOUT: Duration = Duration::from_millis(100);
/// The bytes discarded before a command at most: a port that streams text forever cannot hold
/// up the connection.
const DRAIN_BUDGET: usize = 4096;
/// The longest line read in answer to `N#`, `V#` and `I#`.
const LINE_BUDGET: usize = 256;
/// The inactivity timeout of `V#`, `I#` and `w`.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
const LINE_END: &[u8] = b"\n\r";

/// How the SAM-BA backend connects.
#[derive(Debug, Clone, Default)]
pub struct SambaOptions {
    /// The USB identities the board shows in its bootloader, waited for after the 1200-baud
    /// touch. When none matches the vendor and product of the sketch (bit 15 cleared), the first
    /// is used; when the list is empty, an Arduino sketch's bootloader is its product with bit 15
    /// cleared.
    pub bootloader_devices: Vec<DeviceFilter>,
    /// Ignored: the bootloader talks over USB, whose speed does not depend on a baud rate. It is
    /// reported through `Progress::message` when set.
    pub baud_rate: Option<u32>,
}

/// What `connect` found out, which the later commands need.
struct Session {
    family: &'static Family,
    capabilities: Capabilities,
    /// The lowest flash offset erased by `X` in this session, when any.
    erased_from: Option<u64>,
    /// Whether an image was written in this session: where `X` erases to the end of flash, an
    /// `X` below it would erase it.
    written: bool,
}

/// A connection to an Arduino SAM-BA bootloader.
pub struct SambaBackend<'p, P: PortProvider> {
    /// The open port; `None` while the board re-enumerates and after it failed to reappear.
    port: Option<P::Port>,
    provider: &'p mut P,
    reset: ResetSequence,
    options: SambaOptions,
    /// Set by a successful `connect`.
    session: Option<Session>,
}

fn disconnected() -> Error {
    Error::from(TransportError::Disconnected)
}

/// The filter of the bootloader device to wait for after touching the sketch of `identity`.
fn bootloader_filter(
    identity: &UsbIdentity,
    bootloader_devices: &[DeviceFilter],
) -> Result<DeviceFilter, Error> {
    let product = identity.product & !SKETCH_PRODUCT_BIT;
    let same_product = bootloader_devices
        .iter()
        .find(|filter| filter.vendor == identity.vendor && filter.product == Some(product));
    match same_product.or(bootloader_devices.first()) {
        Some(filter) => Ok(filter.clone()),
        None if identity.vendor == ARDUINO_VENDOR => Ok(DeviceFilter {
            vendor: ARDUINO_VENDOR,
            product: Some(product),
            serial_number: None,
        }),
        None => Err(Error::UnknownBootloaderDevice {
            identity: identity.clone(),
        }),
    }
}

impl<'p, P: PortProvider> SambaBackend<'p, P> {
    /// A backend over `port`, which `provider` can reopen once the board re-enumerates into its
    /// bootloader. Without a `reset`, it uses `touch-1200`; `none` and `custom` also apply, the
    /// Espressif and UF2 resets do not.
    pub fn new(
        port: P::Port,
        provider: &'p mut P,
        reset: Option<ResetSequence>,
        options: SambaOptions,
    ) -> Result<Self, Error> {
        let reset = match reset {
            None => ResetSequence::Touch1200,
            Some(
                sequence @ (ResetSequence::Uf2Touch1200
                | ResetSequence::EspressifClassic
                | ResetSequence::EspressifUsbSerialJtag),
            ) => {
                return Err(Error::UnsupportedReset {
                    protocol: Protocol::Samba,
                    reset: sequence.name().into(),
                });
            }
            Some(sequence) => sequence,
        };
        Ok(SambaBackend {
            port: Some(port),
            provider,
            reset,
            options,
            session: None,
        })
    }

    /// The chip found by the last successful [`connect`](Self::connect).
    pub fn chip(&self) -> Option<&'static str> {
        self.session.as_ref().map(|session| session.family.chip)
    }

    fn port(&mut self) -> Result<&mut P::Port, Error> {
        self.port.as_mut().ok_or_else(disconnected)
    }

    /// Resets the board into its bootloader unless it already sits there, waits for the
    /// bootloader's port, checks that the bootloader has Arduino's extensions and detects the
    /// chip. A requested baud rate is reported through `progress.message` as ignored.
    pub async fn connect(&mut self, progress: &mut dyn Progress) -> Result<ChipInformation, Error> {
        self.session = None;
        self.enter_bootloader().await?;
        if self.options.baud_rate.is_some() {
            progress.message("the SAM-BA bootloader talks over USB: the baud rate is ignored");
        }
        let Some(reply) = self.version().await? else {
            return Err(Error::NoBootloader { waited_for: None });
        };
        let version = command::line(&reply);
        let capabilities = match command::capabilities(&version) {
            Some(found) if found.erase && found.write_buffer && found.checksum => found,
            _ => return Err(Error::UnsupportedBootloader { version }),
        };
        let family = if capabilities.identify {
            self.identify().await?
        } else {
            self.device_identifier().await?
        };
        self.session = Some(Session {
            family,
            capabilities,
            erased_from: None,
            written: false,
        });
        Ok(ChipInformation {
            protocol: Protocol::Samba,
            chip: family.chip.into(),
            flash_size: Some(family.flash_size),
            secure_download_mode: false,
            usb: self.port()?.identity(),
            writable_from: Some(family.application_start),
            erases_to_end: family.erase_to_end,
        })
    }

    /// Touches the sketch and opens the bootloader's port, unless the port is the bootloader's
    /// already: `none` reset, no USB identity (an emulator), a listed bootloader identity with a
    /// product (a vendor alone also matches the sketch), or an Arduino product without bit 15.
    async fn enter_bootloader(&mut self) -> Result<(), Error> {
        let identity = self.port()?.identity();
        let Some(identity) = identity else {
            return Ok(());
        };
        let in_bootloader = self.reset == ResetSequence::None
            || self
                .options
                .bootloader_devices
                .iter()
                .any(|filter| filter.product.is_some() && filter.matches(&identity))
            || (identity.vendor == ARDUINO_VENDOR && identity.product & SKETCH_PRODUCT_BIT == 0);
        if in_bootloader {
            return Ok(());
        }
        let filter = bootloader_filter(&identity, &self.options.bootloader_devices)?;
        match self.touch().await {
            // The sketch already left for the bootloader.
            Ok(())
            | Err(Error::Transport {
                error: TransportError::Disconnected,
                ..
            }) => {}
            Err(error) => return Err(error),
        }
        // The port must be closed before the bootloader's device can be opened.
        drop(self.port.take());
        match self
            .provider
            .open(&filter, BOOTLOADER_BAUD_RATE, REOPEN_TIMEOUT)
            .await
        {
            Ok(port) => {
                self.port = Some(port);
                Ok(())
            }
            Err(TransportError::NotFound { .. }) => Err(Error::NoBootloader {
                waited_for: Some(filter.to_string()),
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Asks the sketch to start the bootloader: 1200 baud with DTR cleared, or the custom steps.
    async fn touch(&mut self) -> Result<(), Error> {
        let Some(port) = self.port.as_mut() else {
            return Err(disconnected());
        };
        match &self.reset {
            ResetSequence::Touch1200 => {
                port.set_baud_rate(TOUCH_BAUD_RATE).await?;
                port.set_signals(Signals {
                    dtr: false,
                    rts: false,
                })
                .await?;
            }
            ResetSequence::Custom(steps) => {
                for step in steps {
                    port.set_signals(Signals {
                        dtr: step.dtr,
                        rts: step.rts,
                    })
                    .await?;
                    if !step.wait.is_zero() {
                        port.sleep(step.wait).await;
                    }
                }
            }
            // The board sits in its bootloader already.
            ResetSequence::None => {}
            // Refused by `new`.
            sequence @ (ResetSequence::Uf2Touch1200
            | ResetSequence::EspressifClassic
            | ResetSequence::EspressifUsbSerialJtag) => {
                return Err(Error::UnsupportedReset {
                    protocol: Protocol::Samba,
                    reset: sequence.name().into(),
                });
            }
        }
        Ok(())
    }

    /// Discards what the port sends until it is quiet, at most [`DRAIN_BUDGET`] bytes.
    async fn drain(&mut self) -> Result<(), Error> {
        let port = self.port()?;
        let mut buffer = [0_u8; READ_CHUNK];
        let mut dropped: usize = 0;
        while dropped < DRAIN_BUDGET {
            let count = port.read(&mut buffer, QUIET_TIMEOUT).await?;
            if count == 0 {
                break;
            }
            dropped = dropped.saturating_add(count);
        }
        Ok(())
    }

    /// Reads until `\n\r`, a read that times out after `timeout` of silence, or `budget`
    /// bytes. Bytes after the first `\n\r` are dropped.
    async fn read_line(&mut self, timeout: Duration, budget: usize) -> Result<Vec<u8>, Error> {
        let port = self.port()?;
        let mut reply = Vec::new();
        let mut buffer = [0_u8; READ_CHUNK];
        while reply.len() < budget {
            let wanted = READ_CHUNK.min(budget.saturating_sub(reply.len()));
            let chunk = buffer.get_mut(..wanted).unwrap_or_default();
            let count = port.read(chunk, timeout).await?.min(wanted);
            if count == 0 {
                break;
            }
            reply.extend_from_slice(chunk.get(..count).unwrap_or_default());
            if let Some(position) = reply
                .windows(LINE_END.len())
                .position(|window| window == LINE_END)
            {
                reply.truncate(position.saturating_add(LINE_END.len()));
                break;
            }
        }
        Ok(reply)
    }

    /// Switches to binary mode and reads the version line, three times at most; `None` when no
    /// attempt got a non-empty line ending in `\n\r`. An empty line is the late answer to `N#`:
    /// the next attempt's drain takes the version line behind it.
    async fn version(&mut self) -> Result<Option<Vec<u8>>, Error> {
        for _ in 0..ATTEMPTS {
            self.drain().await?;
            self.port()?.write(command::BINARY_MODE).await?;
            self.read_line(QUIET_TIMEOUT, LINE_BUDGET).await?;
            self.port()?.write(command::VERSION).await?;
            let reply = self.read_line(COMMAND_TIMEOUT, LINE_BUDGET).await?;
            if reply.ends_with(LINE_END) && !command::line(&reply).is_empty() {
                return Ok(Some(reply));
            }
        }
        Ok(None)
    }

    /// The family of the chip named by `I#` (Arduino extension).
    async fn identify(&mut self) -> Result<&'static Family, Error> {
        self.port()?.write(command::IDENTIFY).await?;
        let reply = self.read_line(COMMAND_TIMEOUT, LINE_BUDGET).await?;
        if !reply.ends_with(LINE_END) {
            return Err(Error::NoResponse { command: "I" });
        }
        let chip = command::line(&reply);
        family::from_identification(&chip).ok_or(Error::UnsupportedChip { chip })
    }

    /// The family of a SAMD21 from its device identifier, read with `w`.
    async fn device_identifier(&mut self) -> Result<&'static Family, Error> {
        let port = self.port()?;
        port.write(&command::read_word(family::DEVICE_IDENTIFIER_ADDRESS))
            .await?;
        let mut word = [0_u8; 4];
        let mut received: usize = 0;
        while received < word.len() {
            let rest = word.get_mut(received..).unwrap_or_default();
            let count = port.read(rest, COMMAND_TIMEOUT).await?.min(rest.len());
            if count == 0 {
                return Err(Error::NoResponse { command: "w" });
            }
            received = received.saturating_add(count);
        }
        let value = u32::from_le_bytes(word);
        family::from_device_identifier(value).ok_or_else(|| Error::UnknownChip {
            detail: format!("device identifier 0x{value:08x}"),
        })
    }
}
