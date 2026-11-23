//! The errors of connecting to a board and writing it, and their classes.

use alloc::string::String;
use core::fmt;

use serde::Serialize;

use crate::manifest::Protocol;
use crate::transport::TransportError;

/// What can go wrong while connecting to a board and writing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Error {
    /// The serial port failed.
    Transport {
        /// What failed.
        error: TransportError,
    },
    /// The board did not answer a command.
    NoResponse {
        /// The command, such as `GET_SECURITY_INFO`.
        command: &'static str,
    },
    /// The board answered no SYNC after any reset.
    NoSync {
        /// The number of resets tried.
        attempts: u8,
    },
    /// The boot log shows the chip started in another mode than download mode.
    NotInDownloadMode {
        /// The boot mode from the log, such as `0x8`.
        boot_mode: String,
    },
    /// The ROM loader refused a command.
    Rom {
        /// The command.
        command: &'static str,
        /// The error code of the ROM loader.
        code: u8,
    },
    /// The board answered a command with something that cannot be its answer.
    InvalidResponse {
        /// The command.
        command: &'static str,
        /// What is wrong with the answer.
        message: String,
    },
    /// The chip could not be identified.
    UnknownChip {
        /// What was read, such as `magic 0x12345678`.
        detail: String,
    },
    /// The chip was identified, and nanoflash does not support it.
    UnsupportedChip {
        /// The chip, such as `esp8266`.
        chip: String,
    },
    /// The reset sequence does not apply to the protocol.
    UnsupportedReset {
        /// The protocol of the board.
        protocol: Protocol,
        /// The reset sequence, such as `touch-1200`.
        reset: String,
    },
    /// A value does not fit what the protocol can carry.
    OutOfRange {
        /// What the value is, such as `application offset`.
        what: String,
        /// The value.
        value: u64,
    },
    /// The flash does not hold what was written.
    VerificationFailed {
        /// The flash offset of the image.
        offset: u64,
        /// The number of bytes checked.
        size: u64,
        /// The MD5 digest of the image, in lowercase hexadecimal.
        expected: String,
        /// The MD5 digest the board computed over the flash, in lowercase hexadecimal.
        actual: String,
    },
}

/// Who can fix an error: the input (exit code 1), the device or its connection (2), or the
/// written data did not read back (3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorClass {
    /// The manifest, the layout, the images or the options.
    Input,
    /// The board, the port or the cable.
    Device,
    /// What was written differs from what reads back.
    Verification,
}

impl Error {
    /// Who can fix this error.
    pub fn class(&self) -> ErrorClass {
        match self {
            Error::UnsupportedReset { .. } | Error::OutOfRange { .. } => ErrorClass::Input,
            Error::Transport { .. }
            | Error::NoResponse { .. }
            | Error::NoSync { .. }
            | Error::NotInDownloadMode { .. }
            | Error::Rom { .. }
            | Error::InvalidResponse { .. }
            | Error::UnknownChip { .. }
            | Error::UnsupportedChip { .. } => ErrorClass::Device,
            Error::VerificationFailed { .. } => ErrorClass::Verification,
        }
    }
}

impl From<TransportError> for Error {
    fn from(error: TransportError) -> Self {
        Error::Transport { error }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport { error } => write!(formatter, "{error}"),
            Error::NoResponse { command } => write!(
                formatter,
                "the board did not answer {command}: check the port and the cable, then try again"
            ),
            Error::NoSync { attempts } => write!(
                formatter,
                "the board did not answer SYNC after {attempts} resets: check the port and the cable, or hold BOOT (GPIO0) while pressing RESET"
            ),
            Error::NotInDownloadMode { boot_mode } => write!(
                formatter,
                "the chip booted in mode {boot_mode} instead of download mode: hold BOOT (GPIO0) while pressing RESET, or check the auto-reset circuit"
            ),
            Error::Rom { command, code } => write!(
                formatter,
                "the ROM loader refused {command}: {} (0x{code:02x})",
                rom_error(*code)
            ),
            Error::InvalidResponse { command, message } => write!(
                formatter,
                "the board answered {command} with an invalid response: {message}"
            ),
            Error::UnknownChip { detail } => write!(
                formatter,
                "the chip could not be identified ({detail}): it may be newer than this version of nanoflash"
            ),
            Error::UnsupportedChip { chip } => {
                write!(formatter, "the {chip} is not supported by nanoflash")
            }
            Error::UnsupportedReset { protocol, reset } => write!(
                formatter,
                "the {reset} reset does not apply to the {protocol} protocol: choose another reset for the board in the manifest"
            ),
            Error::OutOfRange { what, value } => write!(
                formatter,
                "the {what} 0x{value:x} is beyond what the protocol can address"
            ),
            Error::VerificationFailed {
                offset,
                size,
                expected,
                actual,
            } => write!(
                formatter,
                "the 0x{size:x} bytes of flash at 0x{offset:x} have the MD5 digest {actual} instead of {expected}: flash again, and check the power supply and the flash size of the board if it fails again"
            ),
        }
    }
}

impl core::error::Error for Error {}

/// The meaning of an Espressif ROM loader error code.
pub(crate) fn rom_error(code: u8) -> &'static str {
    match code {
        0x05 => "the message is malformed",
        0x06 => "the command failed",
        0x07 => "bad checksum",
        0x08 => "flash write failed (read-back mismatch)",
        0x09 => "flash read failed",
        0x0A => "bad flash read length",
        0x0B => "decompression failed",
        0x0C => "decompressed data has a bad Adler-32 checksum",
        0x0D => "bad decompression parameter",
        0x0E => "bad RAM binary size",
        0x0F => "bad RAM binary address",
        0x64 => "invalid parameter",
        0x65 => "invalid format",
        0x66 => "description too long",
        0x67 => "bad encoding description",
        0x69 => "insufficient storage",
        _ => "unknown error",
    }
}
