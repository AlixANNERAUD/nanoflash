//! The errors of connecting to a board and writing it, and their classes.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde::Serialize;

use crate::layout::LayoutError;
use crate::manifest::{ManifestError, Protocol};
use crate::table::TableError;
use crate::transport::{TransportError, UsbIdentity};

/// What can go wrong while connecting to a board and writing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Error {
    /// The serial port failed.
    Transport {
        /// What failed.
        error: TransportError,
        /// What was being done, such as `writing "application" at 0x20000`, when it is known.
        during: Option<String>,
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
    /// The manifest could not be read.
    Manifest {
        /// What is wrong with it.
        error: ManifestError,
    },
    /// The layout of a board has errors.
    Layout {
        /// Every problem found.
        errors: Vec<LayoutError>,
    },
    /// The layout file of a board could not be read or parsed.
    LayoutFile {
        /// The path, as written in the manifest.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// The partition table cannot be written for the layout.
    Table {
        /// What the writer cannot express.
        error: TableError,
    },
    /// An image file could not be read.
    ImageSource {
        /// The path, as written in the manifest.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// An Intel HEX image is invalid, or does not go where the manifest places it.
    IntelHex {
        /// The path, as written in the manifest, or the file given to the command line.
        path: String,
        /// The 1-based line of the problem, when it is on one line.
        line: Option<usize>,
        /// What is wrong.
        message: String,
    },
    /// An image does not have the SHA-256 digest the manifest gives.
    ChecksumMismatch {
        /// The path, as written in the manifest.
        path: String,
        /// The digest of the manifest, in lowercase hexadecimal.
        expected: String,
        /// The digest of the file, in lowercase hexadecimal.
        actual: String,
    },
    /// The verifier refused an image.
    ImageRejected {
        /// The path, as written in the manifest.
        path: String,
        /// Why.
        reason: String,
    },
    /// An image goes to a partition and the board has no layout.
    ImageNeedsLayout {
        /// The board.
        board: String,
        /// The partition.
        partition: String,
    },
    /// An image or an erase entry names a partition the layout does not define.
    UnknownPartition {
        /// The board.
        board: String,
        /// The partition.
        partition: String,
    },
    /// An image starts at an offset the flash cannot erase from.
    MisalignedImage {
        /// The image.
        label: String,
        /// Its offset.
        offset: u64,
        /// The multiple the offset must be of.
        required: u64,
    },
    /// Two images share flash sectors.
    OverlappingImages {
        /// The image that comes first by offset.
        first: String,
        /// The image that comes second by offset.
        second: String,
        /// The end of the first image, rounded up to a whole erase sector.
        first_end: u64,
        /// The offset of the second image.
        second_offset: u64,
    },
    /// An image does not fit in its partition.
    ImageTooLarge {
        /// The image.
        label: String,
        /// The partition.
        partition: String,
        /// The size of the image.
        image_size: u64,
        /// The size of the partition.
        partition_size: u64,
    },
    /// An image ends after the end of the flash.
    ImageOutsideFlash {
        /// The image.
        label: String,
        /// Its exclusive end.
        end: u64,
        /// The flash size of the layout.
        flash_size: u64,
    },
    /// An erase entry does not start and end on the erase unit of the flash.
    MisalignedErase {
        /// The entry: its partition, or `region at 0x<offset>`.
        entry: String,
        /// Its offset.
        offset: u64,
        /// Its size.
        size: u64,
        /// The multiple the offset and the size must be of.
        required: u64,
    },
    /// An erase entry ends after the end of the flash of the layout.
    EraseOutsideFlash {
        /// The entry: its partition, or `region at 0x<offset>`.
        entry: String,
        /// Its exclusive end.
        end: u64,
        /// The flash size of the layout.
        flash_size: u64,
    },
    /// An image or an erase entry ends beyond what the protocol can address.
    OutsideAddressRange {
        /// What ends there, such as `image "application"`.
        what: String,
        /// Its exclusive end.
        end: u64,
        /// The end of what the protocol can address.
        limit: u64,
        /// The protocol.
        protocol: Protocol,
    },
    /// An image or an erase entry ends after the end of the flash the chip reports.
    OutsideDetectedFlash {
        /// What ends there, such as `image "application"`.
        what: String,
        /// Its exclusive end.
        end: u64,
        /// The flash size the chip reported.
        flash_size: u64,
    },
    /// The board of the manifest lists no image and no erase entry.
    NothingToFlash {
        /// The board.
        board: String,
    },
    /// An image file has no byte.
    EmptyImage {
        /// The path, as written in the manifest.
        path: String,
    },
    /// There is not enough memory for a buffer.
    OutOfMemory {
        /// What the buffer is for, such as `flash image`.
        what: String,
        /// Its size in bytes.
        size: u64,
    },
    /// The requested board is not in the manifest.
    UnknownBoard {
        /// The requested identifier.
        identifier: String,
        /// The identifiers of the manifest.
        known: Vec<String>,
    },
    /// No board of the manifest suits the device.
    NoMatchingBoard {
        /// The USB identity of the device, when it has one.
        identity: Option<UsbIdentity>,
    },
    /// The candidate boards speak several protocols.
    SeveralProtocols {
        /// The protocols.
        protocols: Vec<Protocol>,
    },
    /// Several boards have the detected chip.
    SeveralBoards {
        /// The chip.
        chip: String,
        /// The identifiers of the boards.
        boards: Vec<String>,
    },
    /// The manifest has several boards and none was chosen.
    BoardRequired {
        /// The identifiers of the boards.
        boards: Vec<String>,
    },
    /// The protocol of the device cannot be told from its USB identity and none was given.
    ProtocolRequired {
        /// The USB identity of the device, when it has one.
        identity: Option<UsbIdentity>,
    },
    /// The protocol was not compiled in.
    ProtocolNotEnabled {
        /// The protocol.
        protocol: Protocol,
    },
    /// The size of the flash is needed and neither the layout nor the chip gave it.
    UnknownFlashSize,
    /// The flash of the chip is smaller than the layout.
    FlashTooSmall {
        /// The flash size of the layout.
        layout: u64,
        /// The flash size the chip reported.
        detected: u64,
    },
    /// The chip found is not the one of the selected board.
    WrongChip {
        /// The chip found.
        detected: String,
        /// The chips of the candidate boards.
        expected: Vec<String>,
    },
    /// The flash does not hold what was written.
    VerificationFailed {
        /// The image.
        label: String,
        /// The flash offset of the part checked: the image, or the chunk of it the backend
        /// checks at a time.
        offset: u64,
        /// The number of bytes of the part checked.
        size: u64,
        /// The checksum named by `checksum` of what was written, in lowercase hexadecimal.
        expected: String,
        /// The checksum named by `checksum` the board computed over the flash, in lowercase
        /// hexadecimal.
        actual: String,
        /// The checksum the backend compares, such as `MD5` or `CRC-16`.
        checksum: &'static str,
    },
    /// No bootloader answered: none appeared after the reset, or the port is not one.
    NoBootloader {
        /// The USB identity of the bootloader device waited for after the reset, such as
        /// `2341:005a`; `None` when the port itself did not answer as a bootloader.
        waited_for: Option<String>,
        /// The reset sequence run before waiting, such as `touch-1200` or `custom`; `None` when
        /// nothing was waited for, or for STK500 when the backend pulsed DTR.
        reset: Option<&'static str>,
        /// The protocol of the bootloader, which picks the hint.
        protocol: Protocol,
        /// The baud rates tried on the port, in order; empty for the protocols reached through
        /// the 1200-baud touch, whose USB port ignores the baud rate.
        rates: Vec<u32>,
    },
    /// A backend was asked to erase, write, verify or reset before a successful `connect`.
    NotConnected {
        /// The protocol of the backend.
        protocol: Protocol,
    },
    /// The bootloader lacks the commands nanoflash needs.
    UnsupportedBootloader {
        /// What the bootloader says of its version.
        version: String,
    },
    /// The USB identity the bootloader of a device shows is unknown, so the device cannot be
    /// reopened after the reset.
    UnknownBootloaderDevice {
        /// The USB identity of the device running its application.
        identity: UsbIdentity,
    },
    /// An image or an erase entry starts in the flash the bootloader protects.
    BelowWritableFlash {
        /// What starts there, such as `image "bootloader"`.
        what: String,
        /// Its offset.
        offset: u64,
        /// The lowest flash offset the bootloader writes.
        writable_from: u64,
    },
    /// An image or an erase entry ends in the flash the bootloader occupies at the top of the
    /// flash.
    AboveWritableFlash {
        /// What ends there, such as `image "application"`.
        what: String,
        /// Its end offset.
        end: u64,
        /// The end of the flash the bootloader writes.
        writable_to: u64,
    },
    /// The flash read back differs from what was written.
    ReadBackDiffers {
        /// The image.
        label: String,
        /// The flash offset of the first differing byte.
        offset: u64,
        /// The byte written.
        expected: u8,
        /// The byte read back.
        actual: u8,
    },
    /// An erase region stops before the end of flash, and the bootloader only erases from an
    /// offset to the end of flash.
    EraseShortOfFlashEnd {
        /// The chip, such as `samd21g18a`.
        chip: String,
        /// The erase entry: its partition, or `region at 0x<offset>`.
        entry: String,
        /// The flash offset of the region.
        offset: u64,
        /// Its exclusive end.
        end: u64,
        /// The flash size of the chip, where every erase ends.
        flash_size: u64,
    },
    /// An image comes after an image written above it, and the bootloader erases from the
    /// start of an image to the end of flash before writing it.
    ImageBelowWritten {
        /// The chip, such as `samd21g18a`.
        chip: String,
        /// The image.
        label: String,
        /// Its offset.
        offset: u64,
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
            Error::UnsupportedReset { .. }
            | Error::OutOfRange { .. }
            | Error::Manifest { .. }
            | Error::Layout { .. }
            | Error::LayoutFile { .. }
            | Error::Table { .. }
            | Error::ImageSource { .. }
            | Error::ChecksumMismatch { .. }
            | Error::ImageRejected { .. }
            | Error::IntelHex { .. }
            | Error::ImageNeedsLayout { .. }
            | Error::UnknownPartition { .. }
            | Error::MisalignedImage { .. }
            | Error::OverlappingImages { .. }
            | Error::ImageTooLarge { .. }
            | Error::ImageOutsideFlash { .. }
            | Error::MisalignedErase { .. }
            | Error::EraseOutsideFlash { .. }
            | Error::OutsideAddressRange { .. }
            | Error::OutsideDetectedFlash { .. }
            | Error::NothingToFlash { .. }
            | Error::EmptyImage { .. }
            | Error::OutOfMemory { .. }
            | Error::UnknownBoard { .. }
            | Error::NoMatchingBoard { .. }
            | Error::SeveralProtocols { .. }
            | Error::SeveralBoards { .. }
            | Error::BoardRequired { .. }
            | Error::ProtocolRequired { .. }
            | Error::ProtocolNotEnabled { .. }
            | Error::UnknownFlashSize
            | Error::FlashTooSmall { .. }
            | Error::UnknownBootloaderDevice { .. }
            | Error::BelowWritableFlash { .. }
            | Error::AboveWritableFlash { .. }
            | Error::EraseShortOfFlashEnd { .. }
            | Error::ImageBelowWritten { .. }
            | Error::NotConnected { .. } => ErrorClass::Input,
            Error::Transport { .. }
            | Error::NoResponse { .. }
            | Error::NoSync { .. }
            | Error::NotInDownloadMode { .. }
            | Error::Rom { .. }
            | Error::InvalidResponse { .. }
            | Error::UnknownChip { .. }
            | Error::UnsupportedChip { .. }
            | Error::WrongChip { .. }
            | Error::NoBootloader { .. }
            | Error::UnsupportedBootloader { .. } => ErrorClass::Device,
            Error::VerificationFailed { .. } | Error::ReadBackDiffers { .. } => {
                ErrorClass::Verification
            }
        }
    }
}

impl From<TransportError> for Error {
    fn from(error: TransportError) -> Self {
        Error::Transport {
            error,
            during: None,
        }
    }
}

impl Error {
    /// Says what was being done when the serial port failed, unless the error already says.
    /// Other errors are returned unchanged.
    pub fn during(self, what: impl FnOnce() -> String) -> Error {
        match self {
            Error::Transport {
                error,
                during: None,
            } => Error::Transport {
                error,
                during: Some(what()),
            },
            other => other,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport { error, during } => {
                write!(formatter, "{error}")?;
                if let Some(during) = during {
                    write!(formatter, " while {during}")?;
                }
                if *error == TransportError::Disconnected {
                    formatter.write_str(": check the cable and the port, then try again")?;
                }
                Ok(())
            }
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
            Error::Manifest { error } => write!(formatter, "{error}"),
            Error::Layout { errors } => {
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("\n")?;
                    }
                    write!(formatter, "{error}")?;
                }
                Ok(())
            }
            Error::LayoutFile { path, message } => write!(
                formatter,
                "the layout file \"{path}\" could not be read: {message}: check the \"layout\" path of the board in the manifest"
            ),
            Error::Table { error } => write!(
                formatter,
                "the partition table cannot be written for the layout: {error}"
            ),
            Error::ImageSource { path, message } => write!(
                formatter,
                "the image \"{path}\" could not be read: {message}: check the path in the manifest"
            ),
            Error::ChecksumMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "the image \"{path}\" has the SHA-256 digest {actual} instead of {expected}: rebuild the image or update the \"sha256\" of the manifest"
            ),
            Error::IntelHex {
                path,
                line,
                message,
            } => {
                write!(formatter, "the Intel HEX image \"{path}\" is invalid")?;
                if let Some(line) = line {
                    write!(formatter, " at line {line}")?;
                }
                write!(
                    formatter,
                    ": {message}: rebuild it, or give its binary form"
                )
            }
            Error::ImageRejected { path, reason } => {
                write!(formatter, "the image \"{path}\" was rejected: {reason}")
            }
            Error::ImageNeedsLayout { board, partition } => write!(
                formatter,
                "the board \"{board}\" has a manifest image or erase entry for the partition \"{partition}\" and no layout: add a \"layout\" to the board, or use an \"offset\""
            ),
            Error::UnknownPartition { board, partition } => write!(
                formatter,
                "the board \"{board}\" has a manifest image or erase entry for the partition \"{partition}\", which its layout does not define"
            ),
            Error::MisalignedImage {
                label,
                offset,
                required,
            } => write!(
                formatter,
                "the image \"{label}\" starts at offset 0x{offset:x}, which is not a multiple of 0x{required:x}: the bootloader erases whole sectors of 0x{required:x} bytes, so it would erase the bytes before the image; move the image to a multiple of 0x{required:x}"
            ),
            Error::OverlappingImages {
                first,
                second,
                first_end,
                second_offset,
            } => write!(
                formatter,
                "the image \"{first}\" ends at 0x{first_end:x} once rounded up to whole erase sectors, past the start of the image \"{second}\" at 0x{second_offset:x}: move one of the images"
            ),
            Error::ImageTooLarge {
                label,
                partition,
                image_size,
                partition_size,
            } => write!(
                formatter,
                "the image \"{label}\" of 0x{image_size:x} bytes does not fit in the partition \"{partition}\" of 0x{partition_size:x} bytes: enlarge the partition in the layout or shrink the image"
            ),
            Error::ImageOutsideFlash {
                label,
                end,
                flash_size,
            } => write!(
                formatter,
                "the image \"{label}\" ends at 0x{end:x}, past the end of the 0x{flash_size:x} bytes of flash of the layout: move the image or enlarge the flash in the layout"
            ),
            Error::MisalignedErase {
                entry,
                offset,
                size,
                required,
            } => write!(
                formatter,
                "the erase entry \"{entry}\" covers 0x{offset:x}..0x{:x}, which does not start and end on multiples of 0x{required:x}: the flash is erased in whole sectors of 0x{required:x} bytes, so the bytes around the entry would be erased too; align the entry to 0x{required:x}",
                offset.saturating_add(*size)
            ),
            Error::EraseOutsideFlash {
                entry,
                end,
                flash_size,
            } => write!(
                formatter,
                "the erase entry \"{entry}\" ends at 0x{end:x}, past the end of the 0x{flash_size:x} bytes of flash of the layout: shrink the entry or enlarge the flash in the layout"
            ),
            Error::OutsideAddressRange {
                what,
                end,
                limit,
                protocol,
            } => write!(
                formatter,
                "the {what} ends at 0x{end:x}, past 0x{limit:x}, the end of what the {protocol} protocol can address: move it below 0x{limit:x}"
            ),
            Error::OutsideDetectedFlash {
                what,
                end,
                flash_size,
            } => write!(
                formatter,
                "the {what} ends at 0x{end:x}, past the end of the 0x{flash_size:x} bytes of flash the chip reports, so nothing was erased or written: move it, or connect a board with more flash"
            ),
            Error::NothingToFlash { board } => write!(
                formatter,
                "the board \"{board}\" has no image and no erase entry in the manifest: add an image to write"
            ),
            Error::EmptyImage { path } => write!(
                formatter,
                "the image \"{path}\" is empty: rebuild it, or remove it from the manifest"
            ),
            Error::OutOfMemory { what, size } => write!(
                formatter,
                "there is not enough memory for the 0x{size:x} bytes of the {what}"
            ),
            Error::UnknownBoard { identifier, known } => write!(
                formatter,
                "the manifest has no board \"{identifier}\": the boards are {}",
                known.join(", ")
            ),
            Error::NoMatchingBoard { identity } => {
                match identity {
                    Some(identity) => write!(
                        formatter,
                        "no board of the manifest matches the device {:04x}:{:04x}",
                        identity.vendor, identity.product
                    )?,
                    None => formatter.write_str(
                        "no board of the manifest suits a device without a USB identity",
                    )?,
                }
                formatter.write_str(": pass --board-identifier, or list the device in the manifest")
            }
            Error::SeveralProtocols { protocols } => {
                formatter.write_str("the candidate boards speak several protocols (")?;
                for (index, protocol) in protocols.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{protocol}")?;
                }
                formatter.write_str("): pass --board-identifier")
            }
            Error::SeveralBoards { chip, boards } => write!(
                formatter,
                "several boards have the chip {chip} ({}): pass --board-identifier",
                boards.join(", ")
            ),
            Error::BoardRequired { boards } => write!(
                formatter,
                "the manifest has several boards ({}): pass --board-identifier",
                boards.join(", ")
            ),
            Error::ProtocolRequired { identity } => {
                match identity {
                    Some(identity) => write!(
                        formatter,
                        "the protocol of the device {:04x}:{:04x} is unknown",
                        identity.vendor, identity.product
                    )?,
                    None => formatter.write_str("the protocol of the device is unknown")?,
                }
                formatter.write_str(": pass --protocol")
            }
            Error::ProtocolNotEnabled { protocol } => write!(
                formatter,
                "the {protocol} protocol is not enabled in this build of nanoflash"
            ),
            Error::UnknownFlashSize => formatter.write_str(
                "the size of the flash is unknown: add a layout to the board, or connect to a chip that reports it",
            ),
            Error::FlashTooSmall { layout, detected } => write!(
                formatter,
                "the layout needs 0x{layout:x} bytes of flash and the chip has 0x{detected:x}: use a layout that fits, or a board with more flash"
            ),
            Error::WrongChip { detected, expected } => write!(
                formatter,
                "the chip found is {detected} and the selected boards have {}: pass --board-identifier of the right board, or connect the right board",
                expected.join(", ")
            ),
            Error::VerificationFailed {
                label,
                offset,
                size,
                expected,
                actual,
                checksum,
            } => write!(
                formatter,
                "the image \"{label}\" at 0x{offset:x} (0x{size:x} bytes) does not read back as written: the board computed the {checksum} {actual} instead of {expected}: flash again, and check the power supply and the flash size of the board if it fails again"
            ),
            Error::NoBootloader {
                waited_for,
                reset,
                protocol,
                rates,
            } => match protocol {
                // The Espressif backend reports `NoSync` instead.
                Protocol::Espressif | Protocol::Samba => match (waited_for, reset) {
                    (Some(filter), Some("touch-1200")) => write!(
                        formatter,
                        "no bootloader device matching {filter} appeared within 15 s after the 1200-baud touch: double-tap the reset button of the board (its LED pulses in the bootloader), then try again"
                    ),
                    (Some(filter), Some(reset)) => write!(
                        formatter,
                        "no bootloader device matching {filter} appeared within 15 s after the reset sequence \"{reset}\" of the board: check the reset sequence of the board in the manifest, or double-tap the reset button of the board (its LED pulses in the bootloader) and try again"
                    ),
                    (Some(filter), None) => write!(
                        formatter,
                        "no bootloader device matching {filter} appeared within 15 s after the reset: double-tap the reset button of the board (its LED pulses in the bootloader), then try again"
                    ),
                    (None, _) => formatter.write_str(
                        "no SAM-BA bootloader answered on the port: double-tap the reset button of the board (its LED pulses in the bootloader), then try again with the port it shows",
                    ),
                },
                Protocol::Avr109 => match (waited_for, reset) {
                    (Some(filter), Some("touch-1200")) => write!(
                        formatter,
                        "no bootloader device matching {filter} appeared within 15 s after the 1200-baud touch: press the reset button of the board once (its LED pulses for 8 s in the bootloader), then try again with the port it shows"
                    ),
                    (Some(filter), Some(reset)) => write!(
                        formatter,
                        "no bootloader device matching {filter} appeared within 15 s after the reset sequence \"{reset}\" of the board: check the reset sequence of the board in the manifest, or press the reset button of the board once (its LED pulses for 8 s in the bootloader) and try again with the port it shows"
                    ),
                    (Some(filter), None) => write!(
                        formatter,
                        "no bootloader device matching {filter} appeared within 15 s after the reset: press the reset button of the board once (its LED pulses for 8 s in the bootloader), then try again with the port it shows"
                    ),
                    (None, _) => formatter.write_str(
                        "no AVR109 bootloader answered on the port: press the reset button of the board once (its LED pulses for 8 s in the bootloader), then try again with the port it shows",
                    ),
                },
                Protocol::Stk500v1 | Protocol::Stk500v2 => {
                    let name = if *protocol == Protocol::Stk500v1 {
                        "STK500v1"
                    } else {
                        "STK500v2"
                    };
                    write!(formatter, "no {name} bootloader answered on the port")?;
                    let mut rates = rates.iter();
                    if let Some(first) = rates.next() {
                        write!(formatter, " at {first}")?;
                        for rate in rates {
                            write!(formatter, " and {rate}")?;
                        }
                        formatter.write_str(" baud")?;
                    }
                    if *reset == Some("custom") {
                        formatter.write_str(
                            " after the reset sequence \"custom\" of the board: check the port and the cable and the reset sequence of the board in the manifest, or press the reset button of the board just before flashing",
                        )
                    } else {
                        formatter.write_str(
                            ": check the port and the cable, or press the reset button of the board just before flashing (its automatic reset through DTR may be missing)",
                        )
                    }
                }
            },
            Error::NotConnected { protocol } => write!(
                formatter,
                "the {protocol} backend was asked to erase, write, verify or reset before it connected, so nothing was sent to the board: call connect first"
            ),
            Error::UnsupportedBootloader { version } => write!(
                formatter,
                "the bootloader says \"{version}\" and lacks Arduino's SAM-BA extensions X, Y and Z, which nanoflash needs: update the bootloader, or flash with bossac"
            ),
            Error::UnknownBootloaderDevice { identity } => write!(
                formatter,
                "the USB identity of the bootloader of the device {:04x}:{:04x} is unknown: list it in \"bootloader-devices\" of the board, or double-tap the reset button and pass the bootloader's port",
                identity.vendor, identity.product
            ),
            Error::BelowWritableFlash {
                what,
                offset,
                writable_from,
            } => write!(
                formatter,
                "the {what} starts at 0x{offset:x}, below 0x{writable_from:x}: the bootloader occupies the flash below 0x{writable_from:x} and does not write it; remove it from the manifest or move it to 0x{writable_from:x} or above"
            ),
            Error::AboveWritableFlash {
                what,
                end,
                writable_to,
            } => write!(
                formatter,
                "the {what} ends at 0x{end:x}, past 0x{writable_to:x}: the bootloader occupies the flash from 0x{writable_to:x} and does not write it; remove it from the manifest or make it end at 0x{writable_to:x} or below"
            ),
            Error::ReadBackDiffers {
                label,
                offset,
                expected,
                actual,
            } => write!(
                formatter,
                "the image \"{label}\" does not read back as written: the byte at 0x{offset:x} is 0x{actual:02x} instead of 0x{expected:02x}: flash again; if it fails at the same place, the lock bits of the chip protect that part of the flash"
            ),
            Error::EraseShortOfFlashEnd {
                chip,
                entry,
                offset,
                end,
                flash_size,
            } => write!(
                formatter,
                "the erase entry \"{entry}\" covers 0x{offset:x}..0x{end:x}, and the bootloader of the {chip} erases from an offset to the end of flash, so erasing it would also erase 0x{end:x}..0x{flash_size:x}; it was not erased: make the erase entry end at 0x{flash_size:x}, or remove it from the manifest"
            ),
            Error::ImageBelowWritten {
                chip,
                label,
                offset,
            } => write!(
                formatter,
                "the image \"{label}\" at 0x{offset:x} is written after an image above it, and the bootloader of the {chip} erases from 0x{offset:x} to the end of flash before writing it, which would erase that image, so it was not written: write the images in order of offset"
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn bootloader_errors_have_their_class_and_hint() {
        let waited = Error::NoBootloader {
            waited_for: Some("2341:005a".into()),
            reset: Some("touch-1200"),
            protocol: Protocol::Samba,
            rates: Vec::new(),
        };
        assert_eq!(waited.class(), ErrorClass::Device);
        assert_eq!(
            waited.to_string(),
            "no bootloader device matching 2341:005a appeared within 15 s after the 1200-baud touch: double-tap the reset button of the board (its LED pulses in the bootloader), then try again"
        );
        let custom = Error::NoBootloader {
            waited_for: Some("2341:005a".into()),
            reset: Some("custom"),
            protocol: Protocol::Samba,
            rates: Vec::new(),
        };
        assert_eq!(
            custom.to_string(),
            "no bootloader device matching 2341:005a appeared within 15 s after the reset sequence \"custom\" of the board: check the reset sequence of the board in the manifest, or double-tap the reset button of the board (its LED pulses in the bootloader) and try again"
        );
        let silent = Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Samba,
            rates: Vec::new(),
        };
        assert_eq!(silent.class(), ErrorClass::Device);
        assert_eq!(
            silent.to_string(),
            "no SAM-BA bootloader answered on the port: double-tap the reset button of the board (its LED pulses in the bootloader), then try again with the port it shows"
        );
        let early = Error::NotConnected {
            protocol: Protocol::Samba,
        };
        assert_eq!(early.class(), ErrorClass::Input);
        assert_eq!(
            early.to_string(),
            "the samba backend was asked to erase, write, verify or reset before it connected, so nothing was sent to the board: call connect first"
        );
        let unsupported = Error::UnsupportedBootloader {
            version: "v1.1 [Arduino:IKXYZ]".into(),
        };
        assert_eq!(unsupported.class(), ErrorClass::Device);
        assert_eq!(
            unsupported.to_string(),
            "the bootloader says \"v1.1 [Arduino:IKXYZ]\" and lacks Arduino's SAM-BA extensions X, Y and Z, which nanoflash needs: update the bootloader, or flash with bossac"
        );
        let unknown = Error::UnknownBootloaderDevice {
            identity: UsbIdentity {
                vendor: 0x2341,
                product: 0x805a,
                serial_number: None,
            },
        };
        assert_eq!(unknown.class(), ErrorClass::Input);
        assert_eq!(
            unknown.to_string(),
            "the USB identity of the bootloader of the device 2341:805a is unknown: list it in \"bootloader-devices\" of the board, or double-tap the reset button and pass the bootloader's port"
        );
        let below = Error::BelowWritableFlash {
            what: "image \"bootloader\"".into(),
            offset: 0,
            writable_from: 0x1_0000,
        };
        assert_eq!(below.class(), ErrorClass::Input);
        assert_eq!(
            below.to_string(),
            "the image \"bootloader\" starts at 0x0, below 0x10000: the bootloader occupies the flash below 0x10000 and does not write it; remove it from the manifest or move it to 0x10000 or above"
        );
        let above = Error::AboveWritableFlash {
            what: "image \"application\"".into(),
            end: 0x7E01,
            writable_to: 0x7E00,
        };
        assert_eq!(above.class(), ErrorClass::Input);
        assert_eq!(
            above.to_string(),
            "the image \"application\" ends at 0x7e01, past 0x7e00: the bootloader occupies the flash from 0x7e00 and does not write it; remove it from the manifest or make it end at 0x7e00 or below"
        );
        let short = Error::EraseShortOfFlashEnd {
            chip: "samd21g18a".into(),
            entry: "storage".into(),
            offset: 0x8000,
            end: 0x9000,
            flash_size: 0x4_0000,
        };
        assert_eq!(short.class(), ErrorClass::Input);
        assert_eq!(
            short.to_string(),
            "the erase entry \"storage\" covers 0x8000..0x9000, and the bootloader of the samd21g18a erases from an offset to the end of flash, so erasing it would also erase 0x9000..0x40000; it was not erased: make the erase entry end at 0x40000, or remove it from the manifest"
        );
        let order = Error::ImageBelowWritten {
            chip: "samd21g18a".into(),
            label: "low".into(),
            offset: 0x2000,
        };
        assert_eq!(order.class(), ErrorClass::Input);
        assert_eq!(
            order.to_string(),
            "the image \"low\" at 0x2000 is written after an image above it, and the bootloader of the samd21g18a erases from 0x2000 to the end of flash before writing it, which would erase that image, so it was not written: write the images in order of offset"
        );
    }

    #[test]
    fn read_back_differences_are_verification_failures() {
        let error = Error::ReadBackDiffers {
            label: "application".into(),
            offset: 0x85,
            expected: 0x0f,
            actual: 0xff,
        };
        assert_eq!(error.class(), ErrorClass::Verification);
        assert_eq!(
            error.to_string(),
            "the image \"application\" does not read back as written: the byte at 0x85 is 0xff instead of 0x0f: flash again; if it fails at the same place, the lock bits of the chip protect that part of the flash"
        );
    }

    #[test]
    fn no_bootloader_names_the_protocol_hint() {
        let caterina = Error::NoBootloader {
            waited_for: Some("2341:0036".into()),
            reset: Some("touch-1200"),
            protocol: Protocol::Avr109,
            rates: Vec::new(),
        };
        assert_eq!(caterina.class(), ErrorClass::Device);
        assert_eq!(
            caterina.to_string(),
            "no bootloader device matching 2341:0036 appeared within 15 s after the 1200-baud touch: press the reset button of the board once (its LED pulses for 8 s in the bootloader), then try again with the port it shows"
        );
        let custom = Error::NoBootloader {
            waited_for: Some("2341:0036".into()),
            reset: Some("custom"),
            protocol: Protocol::Avr109,
            rates: Vec::new(),
        };
        assert_eq!(
            custom.to_string(),
            "no bootloader device matching 2341:0036 appeared within 15 s after the reset sequence \"custom\" of the board: check the reset sequence of the board in the manifest, or press the reset button of the board once (its LED pulses for 8 s in the bootloader) and try again with the port it shows"
        );
        let silent = Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Avr109,
            rates: Vec::new(),
        };
        assert_eq!(
            silent.to_string(),
            "no AVR109 bootloader answered on the port: press the reset button of the board once (its LED pulses for 8 s in the bootloader), then try again with the port it shows"
        );
        let optiboot = Error::NoBootloader {
            waited_for: None,
            reset: None,
            protocol: Protocol::Stk500v1,
            rates: alloc::vec![115_200, 57_600],
        };
        assert_eq!(optiboot.class(), ErrorClass::Device);
        assert_eq!(
            optiboot.to_string(),
            "no STK500v1 bootloader answered on the port at 115200 and 57600 baud: check the port and the cable, or press the reset button of the board just before flashing (its automatic reset through DTR may be missing)"
        );
        let wiring = Error::NoBootloader {
            waited_for: None,
            reset: Some("none"),
            protocol: Protocol::Stk500v2,
            rates: alloc::vec![115_200],
        };
        assert_eq!(
            wiring.to_string(),
            "no STK500v2 bootloader answered on the port at 115200 baud: check the port and the cable, or press the reset button of the board just before flashing (its automatic reset through DTR may be missing)"
        );
        let custom = Error::NoBootloader {
            waited_for: None,
            reset: Some("custom"),
            protocol: Protocol::Stk500v1,
            rates: alloc::vec![57_600],
        };
        assert_eq!(
            custom.to_string(),
            "no STK500v1 bootloader answered on the port at 57600 baud after the reset sequence \"custom\" of the board: check the port and the cable and the reset sequence of the board in the manifest, or press the reset button of the board just before flashing"
        );
    }

    #[test]
    fn verification_failures_name_their_checksum() {
        let error = Error::VerificationFailed {
            label: "application".into(),
            offset: 0x1_1000,
            size: 0x1000,
            expected: "d5b0".into(),
            actual: "1234".into(),
            checksum: "CRC-16",
        };
        assert_eq!(error.class(), ErrorClass::Verification);
        assert_eq!(
            error.to_string(),
            "the image \"application\" at 0x11000 (0x1000 bytes) does not read back as written: the board computed the CRC-16 1234 instead of d5b0: flash again, and check the power supply and the flash size of the board if it fails again"
        );
    }
}
