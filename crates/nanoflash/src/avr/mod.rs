// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! The AVR backend: the STK500v1 (Optiboot, ATmegaBOOT), STK500v2 (the wiring bootloader of the
//! Mega 2560) and AVR109 (Caterina) bootloaders of Arduino's boards.
//!
//! STK500v1 and STK500v2 boards are reset into their bootloader by a pulse on DTR and RTS, which
//! the USB-serial chip of the board passes to the RESET pin through a capacitor; AVR109 boards
//! run their USB in the sketch and are reset by the 1200-baud touch.

pub(crate) mod avr109;
pub(crate) mod chip;
pub(crate) mod stk500v1;
pub(crate) mod stk500v2;
mod write;

pub use chip::{CHIPS, Chip};

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::backend::{ChipInformation, Progress};
use crate::error::Error;
use crate::manifest::{Protocol, ResetSequence};
use crate::touch;
use crate::transport::{DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError};
use chip::bootloader_size;

/// The baud rates STK500v1 tries when none is requested: Optiboot's, then ATmegaBOOT's (the old
/// bootloader of the Nano and the Pro Mini).
const STK500V1_RATES: [u32; 2] = [115_200, 57_600];
/// The baud rate of the wiring bootloader of the Mega 2560.
const STK500V2_RATE: u32 = 115_200;
/// The baud rate the AVR109 bootloader's port is opened at; USB ignores it.
const AVR109_RATE: u32 = 57_600;
/// The resets at each baud rate before STK500v1 tries the next one.
const STK500V1_RESETS: u8 = 2;
/// The `STK_GET_SYNC` tries after each reset.
const SYNC_TRIES: u8 = 3;
/// The resets before STK500v2 gives up on `CMD_SIGN_ON`.
const SIGN_ON_ATTEMPTS: u8 = 3;
/// The `S` tries before AVR109 gives up.
const IDENTIFIER_TRIES: u8 = 3;
/// How long DTR and RTS stay cleared before they are asserted, which resets the chip.
const RESET_HOLD: Duration = Duration::from_millis(250);
/// How long to wait after asserting DTR and RTS: 50 ms, as avrdude's `wiring` programmer does
/// (its `arduino` programmer waits 100 ms in avrdude 7). Optiboot waits about 1 s after an
/// external reset before its watchdog starts the application, and every byte it receives
/// restarts that watchdog; with the drain behind this wait, the first sync leaves about 150 ms
/// after the reset, early in that second, which keeps the most of it for the three tries.
const RESET_SETTLE: Duration = Duration::from_millis(50);
const READ_CHUNK: usize = 64;
/// How long the line must stay quiet before the first command after a reset.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
/// How long the line must stay quiet after the sync, to drop the answers of earlier tries.
const SYNCED_DRAIN_TIMEOUT: Duration = Duration::from_millis(50);
/// The bytes a drain discards at most: a sketch that prints forever cannot hold up the
/// connection.
const DRAIN_BUDGET: usize = 4096;
/// The inactivity timeout of each `STK_GET_SYNC` try.
const SYNC_TIMEOUT: Duration = Duration::from_millis(500);
/// The bytes read in answer to one `STK_GET_SYNC` at most.
const SYNC_BUDGET: usize = 64;
/// The inactivity timeout of `CMD_SIGN_ON`.
const SIGN_ON_TIMEOUT: Duration = Duration::from_millis(500);
/// The inactivity timeout of the other commands.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
/// The bytes skipped at most before the start of an STK500v2 message.
const SKIP_BUDGET: usize = 4096;
/// The length of the answer to the AVR109 `S`, such as `CATERIN`.
const IDENTIFIER_LENGTH: usize = 7;
/// The bytes of an answer an error message shows at most.
const EXCERPT_LENGTH: usize = 16;

/// How the AVR backend connects.
#[derive(Debug, Clone, Default)]
pub struct AvrOptions {
    /// On STK500v1 and STK500v2, the baud rate of the bootloader: by default 115200, then 57600
    /// on STK500v1. Ignored on AVR109, whose bootloader talks over USB; it is then reported
    /// through `Progress::message`.
    pub baud_rate: Option<u32>,
    /// On AVR109, the USB identities the board shows in its bootloader, waited for after the
    /// 1200-baud touch. When none matches the sketch's, the first is used; when the list is
    /// empty, the built-in rules of Arduino's and SparkFun's boards apply.
    pub bootloader_devices: Vec<DeviceFilter>,
}

/// The protocols this backend speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Stk500v1,
    Stk500v2,
    Avr109,
}

impl Dialect {
    fn protocol(self) -> Protocol {
        match self {
            Dialect::Stk500v1 => Protocol::Stk500v1,
            Dialect::Stk500v2 => Protocol::Stk500v2,
            Dialect::Avr109 => Protocol::Avr109,
        }
    }
}

/// What `connect` found out, which the later commands need.
struct Session {
    chip: &'static Chip,
    /// The baud rate the bootloader answered at; on AVR109, the rate its port was opened at.
    baud_rate: u32,
}

impl Session {
    /// The end of the flash the bootloader of `protocol` leaves to the application.
    fn writable_to(&self, protocol: Protocol) -> u64 {
        self.chip
            .flash_size
            .saturating_sub(bootloader_size(protocol, self.baud_rate))
    }
}

/// An STK500v2 message read from the port.
enum Frame {
    /// A whole message, from its start byte to its checksum.
    Message(Vec<u8>),
    /// Nothing came.
    Silent,
    /// Bytes came that do not make a message.
    Broken(String),
}

/// Checks an STK500v2 answer and returns its data: [`stk500v2::answer`] or
/// [`stk500v2::answer_with_status`].
type Check = for<'a> fn(&'a [u8], u8, u8) -> Result<&'a [u8], String>;

/// A connection to the bootloader of an Arduino AVR board.
pub struct AvrBackend<'p, P: PortProvider> {
    /// The open port; `None` while the board re-enumerates after the 1200-baud touch, and after
    /// it failed to reappear.
    port: Option<P::Port>,
    provider: &'p mut P,
    dialect: Dialect,
    /// The reset sequence; `None` is the DTR pulse (STK500v1 and STK500v2 only).
    reset: Option<ResetSequence>,
    options: AvrOptions,
    /// The sequence number of the next STK500v2 message.
    sequence: u8,
    /// Set by a successful `connect`.
    session: Option<Session>,
}

fn disconnected() -> Error {
    Error::from(TransportError::Disconnected)
}

/// The bytes in lowercase hexadecimal, separated by spaces.
pub(crate) fn hexadecimal(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(3));
    for (index, byte) in bytes.iter().enumerate() {
        let separator = if index > 0 { " " } else { "" };
        let _ = write!(text, "{separator}{byte:02x}");
    }
    text
}

/// The bytes shown in an error message: in hexadecimal up to [`EXCERPT_LENGTH`] bytes, else the
/// first of them and the count.
pub(crate) fn excerpt(bytes: &[u8]) -> String {
    match bytes.get(..EXCERPT_LENGTH) {
        Some(start) if bytes.len() > EXCERPT_LENGTH => {
            format!("{} ... ({} bytes)", hexadecimal(start), bytes.len())
        }
        _ => hexadecimal(bytes),
    }
}

impl<'p, P: PortProvider> AvrBackend<'p, P> {
    /// A backend speaking `protocol` over `port`, which `provider` can reopen once an AVR109
    /// board re-enumerates into its bootloader. Without a `reset`, STK500v1 and STK500v2 pulse
    /// DTR and RTS and AVR109 uses `touch-1200`; `none` and `custom` apply to the three, and
    /// `touch-1200` to AVR109.
    pub fn new(
        protocol: Protocol,
        port: P::Port,
        provider: &'p mut P,
        reset: Option<ResetSequence>,
        options: AvrOptions,
    ) -> Result<Self, Error> {
        let dialect = match protocol {
            Protocol::Stk500v1 => Dialect::Stk500v1,
            Protocol::Stk500v2 => Dialect::Stk500v2,
            Protocol::Avr109 => Dialect::Avr109,
            Protocol::Espressif | Protocol::Samba => {
                return Err(Error::ProtocolNotEnabled { protocol });
            }
        };
        let reset = match (dialect, reset) {
            (Dialect::Avr109, None) => Some(ResetSequence::Touch1200),
            (_, None) => None,
            (_, Some(sequence @ (ResetSequence::None | ResetSequence::Custom(_))))
            | (Dialect::Avr109, Some(sequence @ ResetSequence::Touch1200)) => Some(sequence),
            (
                _,
                Some(
                    sequence @ (ResetSequence::Touch1200
                    | ResetSequence::Uf2Touch1200
                    | ResetSequence::EspressifClassic
                    | ResetSequence::EspressifUsbSerialJtag),
                ),
            ) => {
                return Err(Error::UnsupportedReset {
                    protocol,
                    reset: sequence.name().into(),
                });
            }
        };
        Ok(AvrBackend {
            port: Some(port),
            provider,
            dialect,
            reset,
            options,
            sequence: 1,
            session: None,
        })
    }

    /// The chip found by the last successful [`connect`](Self::connect).
    pub fn chip(&self) -> Option<&'static Chip> {
        self.session.as_ref().map(|session| session.chip)
    }

    fn port(&mut self) -> Result<&mut P::Port, Error> {
        self.port.as_mut().ok_or_else(disconnected)
    }

    /// Resets the board into its bootloader, synchronises with it and reads the signature of
    /// the chip. STK500v1 tries the requested baud rate, or 115200 then 57600, with two resets
    /// of three `STK_GET_SYNC` each; STK500v2 resets three times for `CMD_SIGN_ON`; AVR109
    /// touches the sketch unless the port is the bootloader's already, and reports a requested
    /// baud rate through `progress.message` as ignored.
    pub async fn connect(&mut self, progress: &mut dyn Progress) -> Result<ChipInformation, Error> {
        self.session = None;
        let (signature, baud_rate) = match self.dialect {
            Dialect::Stk500v1 => self.connect_stk500v1().await?,
            Dialect::Stk500v2 => self.connect_stk500v2().await?,
            Dialect::Avr109 => self.connect_avr109(progress).await?,
        };
        let chip = chip::from_signature(signature).ok_or_else(|| Error::UnknownChip {
            detail: format!("signature {}", hexadecimal(&signature)),
        })?;
        let protocol = self.dialect.protocol();
        let usb = self.port()?.identity();
        let session = self.session.insert(Session { chip, baud_rate });
        Ok(ChipInformation {
            protocol,
            chip: session.chip.name.into(),
            flash_size: Some(session.chip.flash_size),
            secure_download_mode: false,
            usb,
            writable_from: None,
            writable_to: Some(session.writable_to(protocol)),
            erases_to_end: false,
        })
    }

    /// No bootloader answered on the port at `rates`.
    fn no_bootloader(&self, rates: Vec<u32>) -> Error {
        Error::NoBootloader {
            waited_for: None,
            reset: self.reset.as_ref().map(ResetSequence::name),
            protocol: self.dialect.protocol(),
            rates,
        }
    }

    /// Resets the board through DTR and RTS: the pulse, the custom steps, or nothing for `none`.
    async fn reset(&mut self) -> Result<(), Error> {
        let port = self.port.as_mut().ok_or_else(disconnected)?;
        match &self.reset {
            None => {
                port.set_signals(Signals {
                    dtr: false,
                    rts: false,
                })
                .await?;
                port.sleep(RESET_HOLD).await;
                // The lines stay asserted: Web Serial cannot time a release of a few
                // microseconds, and the capacitor to RESET only passes the edge.
                port.set_signals(Signals {
                    dtr: true,
                    rts: true,
                })
                .await?;
                port.sleep(RESET_SETTLE).await;
            }
            Some(ResetSequence::Custom(steps)) => touch::run_steps(port, steps).await?,
            // `none`; the AVR109 touch runs in `touch::enter_bootloader`, and `new` refuses the
            // other sequences.
            Some(_) => {}
        }
        Ok(())
    }

    /// Discards what the port sends until it is quiet for `timeout`, at most [`DRAIN_BUDGET`]
    /// bytes.
    async fn drain(&mut self, timeout: Duration) -> Result<(), Error> {
        let port = self.port()?;
        let mut buffer = [0_u8; READ_CHUNK];
        let mut dropped: usize = 0;
        while dropped < DRAIN_BUDGET {
            let count = port.read(&mut buffer, timeout).await?;
            if count == 0 {
                break;
            }
            dropped = dropped.saturating_add(count);
        }
        Ok(())
    }

    /// Reads up to `length` bytes, stopping early at a read that times out after `timeout` of
    /// silence.
    async fn read_answer(&mut self, length: usize, timeout: Duration) -> Result<Vec<u8>, Error> {
        let port = self.port()?;
        let mut answer = vec![0_u8; length];
        let mut received: usize = 0;
        while received < length {
            let rest = answer.get_mut(received..).unwrap_or_default();
            let count = port.read(rest, timeout).await?.min(rest.len());
            if count == 0 {
                break;
            }
            received = received.saturating_add(count);
        }
        answer.truncate(received);
        Ok(answer)
    }

    async fn connect_stk500v1(&mut self) -> Result<([u8; 3], u32), Error> {
        let rates = match self.options.baud_rate {
            Some(rate) => vec![rate],
            None => STK500V1_RATES.to_vec(),
        };
        for &rate in &rates {
            self.port()?.set_baud_rate(rate).await?;
            for _ in 0..STK500V1_RESETS {
                self.reset().await?;
                self.drain(DRAIN_TIMEOUT).await?;
                if self.sync().await? {
                    // A late answer to an earlier try may follow the one that synced.
                    self.drain(SYNCED_DRAIN_TIMEOUT).await?;
                    return Ok((self.stk500v1_signature().await?, rate));
                }
            }
        }
        Err(self.no_bootloader(rates))
    }

    /// Sends `STK_GET_SYNC` up to [`SYNC_TRIES`] times; whether a try's answer ended with
    /// `14 10`. Each try reads at most [`SYNC_BUDGET`] bytes, so noise ends it.
    async fn sync(&mut self) -> Result<bool, Error> {
        let port = self.port()?;
        for _ in 0..SYNC_TRIES {
            port.write(&stk500v1::GET_SYNC).await?;
            let mut reply = [0_u8; SYNC_BUDGET];
            let mut received: usize = 0;
            while received < SYNC_BUDGET {
                let rest = reply.get_mut(received..).unwrap_or_default();
                let count = port.read(rest, SYNC_TIMEOUT).await?.min(rest.len());
                if count == 0 {
                    break;
                }
                received = received.saturating_add(count);
                if stk500v1::synced(reply.get(..received).unwrap_or_default()) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn stk500v1_signature(&mut self) -> Result<[u8; 3], Error> {
        const COMMAND: &str = "STK_READ_SIGN";
        let data = self
            .stk500v1_command(COMMAND, &stk500v1::READ_SIGNATURE, 3, COMMAND_TIMEOUT)
            .await?;
        <[u8; 3]>::try_from(data.as_slice()).map_err(|_| Error::InvalidResponse {
            command: COMMAND,
            message: format!("expected 3 signature bytes, got {}", hexadecimal(&data)),
        })
    }

    /// Sends the STK500v1 `request` and reads its answer, `14`, `payload` bytes and `10`, at
    /// most `payload + 2` bytes; returns the payload.
    async fn stk500v1_command(
        &mut self,
        command: &'static str,
        request: &[u8],
        payload: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        self.port()?.write(request).await?;
        let reply = self.read_answer(payload.saturating_add(2), timeout).await?;
        if reply.is_empty() {
            return Err(Error::NoResponse { command });
        }
        stk500v1::answer(&reply, payload)
            .map(<[u8]>::to_vec)
            .map_err(|message| Error::InvalidResponse { command, message })
    }

    async fn connect_stk500v2(&mut self) -> Result<([u8; 3], u32), Error> {
        let rate = self.options.baud_rate.unwrap_or(STK500V2_RATE);
        self.port()?.set_baud_rate(rate).await?;
        for _ in 0..SIGN_ON_ATTEMPTS {
            self.reset().await?;
            self.drain(DRAIN_TIMEOUT).await?;
            // The bootloader starts over: number its first message 1.
            self.sequence = 1;
            let answer = self
                .exchange(&stk500v2::SIGN_ON, SIGN_ON_TIMEOUT, stk500v2::answer)
                .await?;
            if let Some(Ok(_)) = answer {
                return Ok((self.stk500v2_signature().await?, rate));
            }
        }
        Err(self.no_bootloader(vec![rate]))
    }

    async fn stk500v2_signature(&mut self) -> Result<[u8; 3], Error> {
        const COMMAND: &str = "CMD_READ_SIGNATURE_ISP";
        let mut signature = [0_u8; 3];
        for (index, byte) in (0_u8..).zip(signature.iter_mut()) {
            let data = self
                .stk500v2_command(
                    COMMAND,
                    &stk500v2::read_signature(index),
                    COMMAND_TIMEOUT,
                    stk500v2::answer_with_status,
                )
                .await?;
            *byte = match data.as_slice() {
                [value] => *value,
                _ => {
                    return Err(Error::InvalidResponse {
                        command: COMMAND,
                        message: format!("expected one signature byte, got {}", excerpt(&data)),
                    });
                }
            };
        }
        Ok(signature)
    }

    /// Sends `body` in the next STK500v2 message and returns the data of the answer as `check`
    /// gives it.
    async fn stk500v2_command(
        &mut self,
        command: &'static str,
        body: &[u8],
        timeout: Duration,
        check: Check,
    ) -> Result<Vec<u8>, Error> {
        match self.exchange(body, timeout, check).await? {
            None => Err(Error::NoResponse { command }),
            Some(Ok(data)) => Ok(data),
            Some(Err(message)) => Err(Error::InvalidResponse { command, message }),
        }
    }

    /// Sends `body` in the next STK500v2 message and returns the data of the answer as `check`
    /// gives it; `None` when nothing came within `timeout`.
    async fn exchange(
        &mut self,
        body: &[u8],
        timeout: Duration,
        check: Check,
    ) -> Result<Option<Result<Vec<u8>, String>>, Error> {
        let sequence = self.sequence;
        self.sequence = sequence.wrapping_add(1);
        let command = body.first().copied().unwrap_or_default();
        self.port()?
            .write(&stk500v2::message(sequence, body))
            .await?;
        Ok(match self.read_message(timeout).await? {
            Frame::Silent => None,
            Frame::Broken(message) => Some(Err(message)),
            Frame::Message(bytes) => Some(check(&bytes, sequence, command).map(<[u8]>::to_vec)),
        })
    }

    /// Reads one STK500v2 message: skips at most [`SKIP_BUDGET`] bytes until its start, then
    /// reads its header and as many bytes as it announces, never past its end.
    async fn read_message(&mut self, timeout: Duration) -> Result<Frame, Error> {
        let port = self.port()?;
        let mut buffer = [0_u8; READ_CHUNK];
        let mut skipped: usize = 0;
        let mut message: Vec<u8> = Vec::new();
        // The length of the whole message, once its header is in.
        let mut length: Option<usize> = None;
        loop {
            // Before the start, the shortest message (a header and a checksum) is read at most:
            // whatever follows a start byte in it is still part of that message.
            let wanted = match length {
                Some(length) => length.saturating_sub(message.len()),
                None if message.is_empty() => stk500v2::HEADER_LENGTH + 1,
                None => stk500v2::HEADER_LENGTH.saturating_sub(message.len()),
            }
            .min(READ_CHUNK);
            let chunk = buffer.get_mut(..wanted).unwrap_or_default();
            let count = port.read(chunk, timeout).await?.min(wanted);
            if count == 0 {
                return Ok(if message.is_empty() && skipped == 0 {
                    Frame::Silent
                } else if message.is_empty() {
                    Frame::Broken(format!(
                        "{skipped} bytes came without the start of a message (1b)"
                    ))
                } else {
                    Frame::Broken(format!(
                        "the message stopped after {} bytes: {}",
                        message.len(),
                        excerpt(&message)
                    ))
                });
            }
            let mut bytes = chunk.get(..count).unwrap_or_default();
            if message.is_empty() {
                let start = bytes.iter().position(|byte| *byte == stk500v2::START);
                skipped = skipped.saturating_add(start.unwrap_or(count));
                if skipped > SKIP_BUDGET {
                    return Ok(Frame::Broken(format!(
                        "more than {SKIP_BUDGET} bytes came without the start of a message (1b)"
                    )));
                }
                let Some(start) = start else {
                    continue;
                };
                bytes = bytes.get(start..).unwrap_or_default();
            }
            message.extend_from_slice(bytes);
            if length.is_none() {
                if let Some(header) = message
                    .get(..stk500v2::HEADER_LENGTH)
                    .and_then(|header| <[u8; stk500v2::HEADER_LENGTH]>::try_from(header).ok())
                {
                    let Some(whole) = stk500v2::message_length(header) else {
                        return Ok(Frame::Broken(format!(
                            "the message header {} announces a body over {} bytes",
                            hexadecimal(&header),
                            stk500v2::MAXIMUM_BODY
                        )));
                    };
                    length = Some(whole);
                }
            }
            if let Some(length) = length {
                if message.len() >= length {
                    message.truncate(length);
                    return Ok(Frame::Message(message));
                }
            }
        }
    }

    async fn connect_avr109(
        &mut self,
        progress: &mut dyn Progress,
    ) -> Result<([u8; 3], u32), Error> {
        const COMMAND: &str = "AVR109 s";
        if self.options.baud_rate.is_some() {
            progress.message("AVR109 bootloaders talk over USB: the baud rate is ignored");
        }
        let touch = ResetSequence::Touch1200;
        let reset = self.reset.as_ref().unwrap_or(&touch);
        touch::enter_bootloader(
            &mut self.port,
            &mut *self.provider,
            reset,
            &self.options.bootloader_devices,
            Protocol::Avr109,
            AVR109_RATE,
        )
        .await?;
        self.drain(DRAIN_TIMEOUT).await?;
        let mut identified = false;
        for _ in 0..IDENTIFIER_TRIES {
            self.port()?.write(&[avr109::IDENTIFIER]).await?;
            let reply = self.read_answer(IDENTIFIER_LENGTH, COMMAND_TIMEOUT).await?;
            if reply.len() == IDENTIFIER_LENGTH && reply.first() != Some(&avr109::UNKNOWN) {
                identified = true;
                break;
            }
        }
        if !identified {
            return Err(Error::NoBootloader {
                waited_for: None,
                reset: None,
                protocol: Protocol::Avr109,
                rates: Vec::new(),
            });
        }
        self.port()?.write(&[avr109::SIGNATURE]).await?;
        let reply = self.read_answer(3, COMMAND_TIMEOUT).await?;
        match <[u8; 3]>::try_from(reply.as_slice()) {
            Ok(reply) => Ok((avr109::signature(reply), AVR109_RATE)),
            Err(_) if reply.is_empty() => Err(Error::NoResponse { command: COMMAND }),
            Err(_) => Err(Error::InvalidResponse {
                command: COMMAND,
                message: format!("expected 3 signature bytes, got {}", hexadecimal(&reply)),
            }),
        }
    }
}
