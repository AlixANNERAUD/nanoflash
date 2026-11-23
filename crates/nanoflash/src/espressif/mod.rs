//! The Espressif ROM loader backend: reset into download mode, synchronise, detect the chip and
//! its flash, over SLIP-framed commands.

pub(crate) mod chip;
pub(crate) mod command;
pub(crate) mod reset;
pub(crate) mod slip;

pub use chip::Chip;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::backend::{ChipInformation, Progress};
use crate::error::Error;
use crate::manifest::{Protocol, ResetSequence};
use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, TransportError, UsbIdentity,
};
use command::{Response, opcode, words};

/// The baud rate the ROM loader starts at.
const ROM_BAUD_RATE: u32 = 115_200;
/// The baud rate used through a UART bridge when none is requested.
const BRIDGE_BAUD_RATE: u32 = 460_800;
/// SYNC requests sent after each reset.
const SYNC_TRIES: u8 = 5;
const SYNC_TIMEOUT: Duration = Duration::from_millis(100);
/// How long the line must stay quiet once SYNC is answered.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const REOPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// The bytes a command reads without finding its answer before it gives up.
const BYTE_BUDGET: usize = 1024 * 1024;
const READ_CHUNK: usize = 64;
/// The flash size given to `SPI_SET_PARAMS` when it could not be read.
const DEFAULT_FLASH_SIZE: u32 = 4 * 1024 * 1024;
/// The USB identity of the USB-Serial-JTAG peripheral.
const USB_SERIAL_JTAG_VENDOR: u16 = 0x303A;
const USB_SERIAL_JTAG_PRODUCT: u16 = 0x1001;
/// Bit 2 of the `GET_SECURITY_INFO` flags: secure download mode.
const SECURE_DOWNLOAD_FLAG: u32 = 1 << 2;
/// SPI user register bits: a command phase and a MISO phase.
const SPI_USER_COMMAND_AND_MISO: u32 = 1 << 31 | 1 << 28;
/// SPI user2 register: an 8-bit command (`7` is the length minus one), JEDEC `RDID` (`0x9F`).
const SPI_READ_IDENTIFIER: u32 = 7 << 28 | 0x9F;
/// The 24 bits of the JEDEC identifier, minus one.
const SPI_IDENTIFIER_BITS: u32 = 23;
/// The command register bit that starts a user command and clears when it is done.
const SPI_USER_START: u32 = 1 << 18;
const SPI_POLLS: u8 = 10;

/// How the Espressif backend connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspressifOptions {
    /// The baud rate to switch to after connecting. `None` means 460800 through a UART bridge,
    /// and 115200 on USB-Serial-JTAG and the ESP32-C2.
    pub baud_rate: Option<u32>,
    /// How many resets to try before giving up.
    pub connect_attempts: u8,
}

impl Default for EspressifOptions {
    fn default() -> Self {
        EspressifOptions {
            baud_rate: None,
            connect_attempts: 4,
        }
    }
}

/// A connection to the ROM loader of an Espressif chip.
pub struct EspressifBackend<'p, P: PortProvider> {
    port: P::Port,
    provider: &'p mut P,
    reset: ResetSequence,
    options: EspressifOptions,
    decoder: slip::Decoder,
    /// Frames decoded and not yet examined.
    frames: VecDeque<Vec<u8>>,
    chip: Option<Chip>,
}

fn is_usb_serial_jtag(identity: &UsbIdentity) -> bool {
    identity.vendor == USB_SERIAL_JTAG_VENDOR && identity.product == USB_SERIAL_JTAG_PRODUCT
}

fn disconnected() -> Error {
    Error::from(TransportError::Disconnected)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The boot mode of a boot log that did not reach download mode, such as `0x8`.
fn wrong_boot_mode(log: &[u8]) -> Option<String> {
    if find(log, b"waiting for download").is_some() {
        return None;
    }
    let start = find(log, b"boot:0x")?.checked_add(5)?;
    let rest = log.get(start..)?;
    let digits = rest
        .get(2..)?
        .iter()
        .take_while(|byte| byte.is_ascii_hexdigit())
        .count();
    if digits == 0 {
        return None;
    }
    let mode = rest.get(..digits.checked_add(2)?)?;
    Some(mode.iter().map(|&byte| char::from(byte)).collect())
}

/// What to do with the baud rate once connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaudRatePlan {
    /// Stay at 115200 baud.
    Keep,
    /// Stay at 115200 baud although this rate was requested, for this reason.
    Refuse(u32, &'static str),
    /// Switch to this rate.
    Change(u32),
}

fn plan_baud_rate(requested: Option<u32>, chip: Chip, usb_serial_jtag: bool) -> BaudRatePlan {
    let fixed = if usb_serial_jtag {
        Some("the USB-Serial-JTAG peripheral does not use a baud rate")
    } else if chip == Chip::Esp32C2 {
        Some("the ESP32-C2 ROM loader assumes a 40 MHz crystal, which many boards do not have")
    } else {
        None
    };
    match (requested, fixed) {
        (Some(ROM_BAUD_RATE), _) | (None, Some(_)) => BaudRatePlan::Keep,
        (Some(rate), Some(reason)) => BaudRatePlan::Refuse(rate, reason),
        (Some(rate), None) => BaudRatePlan::Change(rate),
        (None, None) => BaudRatePlan::Change(BRIDGE_BAUD_RATE),
    }
}

impl<'p, P: PortProvider> EspressifBackend<'p, P> {
    /// A backend over `port`, which `provider` can reopen when the device re-enumerates. Without
    /// a `reset`, it uses `espressif-usb-serial-jtag` for the USB identity `303a:1001` and
    /// `espressif-classic` otherwise. The 1200-baud touch resets do not apply.
    pub fn new(
        port: P::Port,
        provider: &'p mut P,
        reset: Option<ResetSequence>,
        options: EspressifOptions,
    ) -> Result<Self, Error> {
        let reset = match reset {
            Some(sequence @ (ResetSequence::Touch1200 | ResetSequence::Uf2Touch1200)) => {
                return Err(reset::unsupported(&sequence));
            }
            Some(sequence) => sequence,
            None if port.identity().as_ref().is_some_and(is_usb_serial_jtag) => {
                ResetSequence::EspressifUsbSerialJtag
            }
            None => ResetSequence::EspressifClassic,
        };
        Ok(EspressifBackend {
            port,
            provider,
            reset,
            options,
            decoder: slip::Decoder::new(),
            frames: VecDeque::new(),
            chip: None,
        })
    }

    /// The chip found by the last successful [`connect`](Self::connect).
    pub fn chip(&self) -> Option<Chip> {
        self.chip
    }

    /// Resets the board into download mode, synchronises with the ROM loader, detects the chip
    /// and its flash, and switches to the requested baud rate. Settings it cannot apply are
    /// reported through `progress.message`.
    pub async fn connect(&mut self, progress: &mut dyn Progress) -> Result<ChipInformation, Error> {
        self.chip = None;
        let mut reopened = false;
        let mut synced = false;
        for attempt in 0..self.options.connect_attempts {
            self.decoder = slip::Decoder::new();
            self.frames.clear();
            match reset::enter_download_mode(&mut self.port, &self.reset, attempt).await {
                Err(Error::Transport {
                    error: TransportError::Disconnected,
                }) => self.reopen(&mut reopened).await?,
                other => other?,
            }
            if self.synchronise(&mut reopened).await? {
                synced = true;
                break;
            }
        }
        if !synced {
            let log = self.decoder.take_noise();
            return Err(match wrong_boot_mode(&log) {
                Some(boot_mode) => Error::NotInDownloadMode { boot_mode },
                None => Error::NoSync {
                    attempts: self.options.connect_attempts,
                },
            });
        }
        self.drain().await?;
        self.detect(progress).await
    }

    /// Opens the device again after it left, once per connection, when it has an identity.
    async fn reopen(&mut self, reopened: &mut bool) -> Result<(), Error> {
        if *reopened {
            return Err(disconnected());
        }
        let Some(identity) = self.port.identity() else {
            return Err(disconnected());
        };
        *reopened = true;
        self.port = self
            .provider
            .open(
                &DeviceFilter::exact(&identity),
                ROM_BAUD_RATE,
                REOPEN_TIMEOUT,
            )
            .await?;
        self.decoder = slip::Decoder::new();
        self.frames.clear();
        Ok(())
    }

    /// Sends SYNC up to five times; `true` when the ROM loader answered.
    async fn synchronise(&mut self, reopened: &mut bool) -> Result<bool, Error> {
        let mut tries = 0;
        while tries < SYNC_TRIES {
            match self
                .exchange(opcode::SYNC, &command::SYNC_DATA, SYNC_TIMEOUT)
                .await
            {
                Ok(Some(response)) if response.status == 0 => return Ok(true),
                Ok(_) | Err(Error::NoResponse { .. }) => tries += 1,
                Err(Error::Transport {
                    error: TransportError::Disconnected,
                }) => {
                    // A fresh port gets every try again; `reopen` only succeeds once.
                    self.reopen(reopened).await?;
                    tries = 0;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }

    /// Discards the extra SYNC replies and anything else until the line is quiet.
    async fn drain(&mut self) -> Result<(), Error> {
        let mut received: usize = 0;
        loop {
            self.frames.clear();
            if received >= BYTE_BUDGET {
                return Err(Error::InvalidResponse {
                    command: command::name(opcode::SYNC),
                    message: "the board kept sending data after its answer".into(),
                });
            }
            let count = self.read_chunk(DRAIN_TIMEOUT).await?;
            if count == 0 {
                return Ok(());
            }
            received = received.saturating_add(count);
        }
    }

    /// Reads once into the decoder, queueing the frames it completes; returns the byte count.
    async fn read_chunk(&mut self, timeout: Duration) -> Result<usize, Error> {
        let mut buffer = [0_u8; READ_CHUNK];
        let count = self.port.read(&mut buffer, timeout).await?.min(READ_CHUNK);
        for &byte in buffer.get(..count).unwrap_or_default() {
            if let Some(frame) = self.decoder.push(byte) {
                self.frames.push_back(frame);
            }
        }
        Ok(count)
    }

    /// Sends a command and waits for the response with its opcode, skipping other frames.
    /// `None` means a read timed out first; a spent byte budget is `NoResponse`.
    async fn exchange(
        &mut self,
        code: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<Option<Response>, Error> {
        self.port
            .write(&slip::encode(&command::request(code, data, 0)))
            .await?;
        let mut received: usize = 0;
        loop {
            while let Some(frame) = self.frames.pop_front() {
                match command::parse_response(&frame) {
                    Some(response) if response.opcode == code => return Ok(Some(response)),
                    _ => {}
                }
            }
            if received >= BYTE_BUDGET {
                return Err(Error::NoResponse {
                    command: command::name(code),
                });
            }
            let count = self.read_chunk(timeout).await?;
            if count == 0 {
                return Ok(None);
            }
            received = received.saturating_add(count);
        }
    }

    /// Sends a command the ROM loader must answer and accept.
    async fn command(&mut self, code: u8, data: &[u8]) -> Result<Response, Error> {
        let command = command::name(code);
        match self.exchange(code, data, COMMAND_TIMEOUT).await? {
            None => Err(Error::NoResponse { command }),
            Some(response) if response.status != 0 => Err(Error::Rom {
                command,
                code: response.error,
            }),
            Some(response) => Ok(response),
        }
    }

    async fn read_register(&mut self, address: u32) -> Result<u32, Error> {
        let response = self.command(opcode::READ_REG, &words(&[address])).await?;
        Ok(response.value)
    }

    async fn write_register(&mut self, address: u32, value: u32) -> Result<(), Error> {
        self.command(opcode::WRITE_REG, &words(&[address, value, 0xFFFF_FFFF, 0]))
            .await?;
        Ok(())
    }

    async fn detect(&mut self, progress: &mut dyn Progress) -> Result<ChipInformation, Error> {
        let identity = self.port.identity();
        let usb_serial_jtag = identity.as_ref().is_some_and(is_usb_serial_jtag);
        let (chip_id, secure_download_mode) = self.security_info().await?;
        let chip = match chip_id.and_then(Chip::from_chip_id) {
            Some(chip) => chip,
            None => self.chip_from_magic(chip_id).await?,
        };
        if usb_serial_jtag && !secure_download_mode {
            // The RTC watchdog would reset the chip in the middle of the session: the ROM leaves
            // it running when booted from USB-Serial-JTAG.
            if let Some(watchdog) = chip.watchdog() {
                self.write_register(watchdog.write_protect, chip::WATCHDOG_KEY)
                    .await?;
                self.write_register(watchdog.configuration, 0).await?;
                self.write_register(watchdog.write_protect, 0).await?;
            }
        }
        self.command(opcode::SPI_ATTACH, &words(&[0, 0])).await?;
        let flash_size = if secure_download_mode {
            None
        } else {
            self.flash_size(chip, progress).await?
        };
        let parameter_size = flash_size
            .and_then(|size| u32::try_from(size).ok())
            .unwrap_or(DEFAULT_FLASH_SIZE);
        self.command(
            opcode::SPI_SET_PARAMS,
            &words(&[0, parameter_size, 0x1_0000, 0x1000, 0x100, 0xFFFF]),
        )
        .await?;
        match plan_baud_rate(self.options.baud_rate, chip, usb_serial_jtag) {
            BaudRatePlan::Keep => {}
            BaudRatePlan::Refuse(rate, reason) => {
                progress.message(&format!("keeping 115200 baud instead of {rate}: {reason}"))
            }
            BaudRatePlan::Change(rate) => {
                self.command(opcode::CHANGE_BAUDRATE, &words(&[rate, 0]))
                    .await?;
                self.port.set_baud_rate(rate).await?;
                self.port.sleep(Duration::from_millis(50)).await;
            }
        }
        self.chip = Some(chip);
        Ok(ChipInformation {
            protocol: Protocol::Espressif,
            chip: chip.name().into(),
            flash_size,
            secure_download_mode,
            usb: identity,
        })
    }

    /// The chip identifier and the secure download mode flag from `GET_SECURITY_INFO`. ROMs
    /// that predate it (ESP32, ESP8266) refuse it or stay silent: no chip identifier, not
    /// secure. A spent byte budget and transport errors stand.
    async fn security_info(&mut self) -> Result<(Option<u32>, bool), Error> {
        let response = match self
            .exchange(opcode::GET_SECURITY_INFO, &[], COMMAND_TIMEOUT)
            .await?
        {
            Some(response) if response.status == 0 => response,
            _ => return Ok((None, false)),
        };
        let word = |start: usize| {
            let bytes = response.data.get(start..start.checked_add(4)?)?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        };
        if response.data.len() < 12 {
            return Ok((None, false));
        }
        let secure = word(0).is_some_and(|flags| flags & SECURE_DOWNLOAD_FLAG != 0);
        let chip_id = if response.data.len() >= 20 {
            word(12)
        } else {
            None
        };
        Ok((chip_id, secure))
    }

    /// Identifies the chip by the magic value of its ROM.
    async fn chip_from_magic(&mut self, chip_id: Option<u32>) -> Result<Chip, Error> {
        let magic = self.read_register(chip::MAGIC_ADDRESS).await?;
        if magic == chip::ESP8266_MAGIC {
            return Err(Error::UnsupportedChip {
                chip: "esp8266".into(),
            });
        }
        Chip::from_magic(magic).ok_or_else(|| Error::UnknownChip {
            detail: match chip_id {
                Some(id) => format!("chip identifier {id}, magic 0x{magic:08x}"),
                None => format!("magic 0x{magic:08x}"),
            },
        })
    }

    /// The flash size from its JEDEC identifier; `None`, with a message, when it is unknown.
    async fn flash_size(
        &mut self,
        chip: Chip,
        progress: &mut dyn Progress,
    ) -> Result<Option<u64>, Error> {
        let identifier = self.flash_identifier(chip).await?;
        let [_, _, size_identifier, _] = identifier.to_le_bytes();
        let size = chip::flash_size(size_identifier);
        if size.is_none() {
            progress.message(&format!(
                "the flash identifier 0x{:06x} names no known size: assuming 4 MiB",
                identifier & 0x00FF_FFFF
            ));
        }
        Ok(size)
    }

    /// Runs the JEDEC `RDID` command through the SPI registers, restoring them afterwards.
    async fn flash_identifier(&mut self, chip: Chip) -> Result<u32, Error> {
        let registers = chip.spi_registers();
        let old_user = self.read_register(registers.user).await?;
        let old_user2 = self.read_register(registers.user2).await?;
        self.write_register(registers.user, SPI_USER_COMMAND_AND_MISO)
            .await?;
        self.write_register(registers.user2, SPI_READ_IDENTIFIER)
            .await?;
        self.write_register(registers.miso_length, SPI_IDENTIFIER_BITS)
            .await?;
        self.write_register(registers.data, 0).await?;
        self.write_register(registers.command, SPI_USER_START)
            .await?;
        let mut done = false;
        for _ in 0..SPI_POLLS {
            self.port.sleep(Duration::from_millis(1)).await;
            if self.read_register(registers.command).await? & SPI_USER_START == 0 {
                done = true;
                break;
            }
        }
        if !done {
            return Err(Error::NoResponse {
                command: "SPI flash identifier",
            });
        }
        let identifier = self.read_register(registers.data).await?;
        self.write_register(registers.user, old_user).await?;
        self.write_register(registers.user2, old_user2).await?;
        Ok(identifier)
    }
}

#[cfg(test)]
mod tests;
