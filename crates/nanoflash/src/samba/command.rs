//! Encoding of SAM-BA commands, the extensions Arduino's bootloaders announce, and the CRC.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Switch the monitor to binary mode.
pub(crate) const BINARY_MODE: &[u8] = b"N#";
/// Ask for the version line.
pub(crate) const VERSION: &[u8] = b"V#";
/// Ask for the identification line (Arduino extension `I`).
pub(crate) const IDENTIFY: &[u8] = b"I#";
/// Leave the bootloader and start the application (Arduino extension `K`).
pub(crate) const RESET: &[u8] = b"K#";

/// Erase from `address` (Arduino extension `X`).
pub(crate) fn erase(address: u32) -> Vec<u8> {
    format!("X{address:08X}#").into_bytes()
}

/// Announce `size` bytes to store in RAM at `buffer`; the bytes follow.
pub(crate) fn send(buffer: u32, size: u32) -> Vec<u8> {
    format!("S{buffer:08X},{size:08X}#").into_bytes()
}

/// Select the RAM buffer the next copy reads from (Arduino extension `Y`).
pub(crate) fn select_buffer(buffer: u32) -> Vec<u8> {
    format!("Y{buffer:08X},0#").into_bytes()
}

/// Copy `size` bytes of the selected buffer to flash at `address` (Arduino extension `Y`).
pub(crate) fn copy(address: u32, size: u32) -> Vec<u8> {
    format!("Y{address:08X},{size:08X}#").into_bytes()
}

/// Ask for the CRC-16 of `size` bytes of flash at `address` (Arduino extension `Z`).
pub(crate) fn checksum(address: u32, size: u32) -> Vec<u8> {
    format!("Z{address:08X},{size:08X}#").into_bytes()
}

/// Read the 32-bit word at `address`.
pub(crate) fn read_word(address: u32) -> Vec<u8> {
    format!("w{address:08X},4#").into_bytes()
}

/// Write the 32-bit word `value` at `address`.
pub(crate) fn write_word(address: u32, value: u32) -> Vec<u8> {
    format!("W{address:08X},{value:08X}#").into_bytes()
}

/// The extensions a bootloader announces between `[Arduino:` and `]` in its version line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Capabilities {
    /// `I`: identification line.
    pub identify: bool,
    /// `K`: reset command.
    pub reset: bool,
    /// `X`: erase command.
    pub erase: bool,
    /// `Y`: copy from the RAM buffer to flash.
    pub write_buffer: bool,
    /// `Z`: CRC-16 of flash.
    pub checksum: bool,
}

/// Read the extensions of a version line, or `None` when it announces none (not an Arduino
/// bootloader, or the closing `]` is missing). Unknown letters such as `P` are ignored.
pub(crate) fn capabilities(version: &str) -> Option<Capabilities> {
    const OPENING: &str = "[Arduino:";
    let start = version.find(OPENING)? + OPENING.len();
    let letters = version.get(start..)?;
    let letters = &letters[..letters.find(']')?];
    let mut found = Capabilities::default();
    for letter in letters.chars() {
        match letter {
            'I' => found.identify = true,
            'K' => found.reset = true,
            'X' => found.erase = true,
            'Y' => found.write_buffer = true,
            'Z' => found.checksum = true,
            _ => {}
        }
    }
    Some(found)
}

/// The bytes of `reply` before the first non-printable one, as text (the line without its
/// `\n\r`).
pub(crate) fn line(reply: &[u8]) -> String {
    reply
        .iter()
        .take_while(|byte| (0x20..0x7F).contains(*byte))
        .map(|&byte| char::from(byte))
        .collect()
}

/// Read the reply of `Z`: `Z`, eight hexadecimal digits, `#`, optionally followed by `\n\r`.
/// The value must fit the 16 bits of the CRC.
pub(crate) fn parse_checksum(reply: &[u8]) -> Option<u16> {
    let rest = reply.strip_prefix(b"Z")?;
    let digits = rest.get(..8)?;
    if rest.get(8) != Some(&b'#') || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value = u32::from_str_radix(core::str::from_utf8(digits).ok()?, 16).ok()?;
    u16::try_from(value).ok()
}

/// The CRC-16/XMODEM of `data` (polynomial `0x1021`, initial value 0, no reflection, no final
/// XOR): what the bootloader's `Z` command answers for the same bytes of flash.
///
/// ```
/// # #[cfg(feature = "samba")] {
/// assert_eq!(nanoflash::samba::crc16(b"123456789"), 0x31C3);
/// # }
/// ```
pub fn crc16(data: &[u8]) -> u16 {
    const XMODEM: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_XMODEM);
    XMODEM.checksum(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_commands_as_bossac_sends_them() {
        assert_eq!(erase(0), b"X00000000#");
        assert_eq!(send(0x34, 0x1000), b"S00000034,00001000#");
        assert_eq!(select_buffer(0x34), b"Y00000034,0#");
        assert_eq!(copy(0x1A000, 0x1000), b"Y0001A000,00001000#");
        assert_eq!(checksum(0x25000, 0x800), b"Z00025000,00000800#");
        assert_eq!(read_word(0x4100_2018), b"w41002018,4#");
        assert_eq!(write_word(0xE000_ED0C, 0x05FA_0004), b"WE000ED0C,05FA0004#");
    }

    #[test]
    fn reads_the_extensions() {
        let all = capabilities("Arduino Bootloader (SAM-BA extended) 2.0 [Arduino:IKXYZ]");
        assert_eq!(
            all,
            Some(Capabilities {
                identify: true,
                reset: true,
                erase: true,
                write_buffer: true,
                checksum: true
            })
        );
        let samd = capabilities("v2.0 [Arduino:XYZP] Nov 23 2026 12:00:00");
        assert_eq!(
            samd,
            Some(Capabilities {
                identify: false,
                reset: false,
                erase: true,
                write_buffer: true,
                checksum: true
            })
        );
        assert_eq!(capabilities("v1.1 Dec 15 2010"), None);
        assert_eq!(capabilities("[Arduino:XYZ"), None);
    }

    #[test]
    fn crc16_is_xmodem() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(crc16(&[]), 0);
        assert_eq!(crc16(&[0xFF; 4096]), 0xE03E);
    }

    #[test]
    fn parses_checksum_replies() {
        assert_eq!(parse_checksum(b"Z0000D5B0#\n\r"), Some(0xD5B0));
        assert_eq!(parse_checksum(b"Z0000d5b0#"), Some(0xD5B0));
        assert_eq!(parse_checksum(b"Z0001D5B0#"), None);
        assert_eq!(parse_checksum(b"Z00D5B0#"), None);
        assert_eq!(parse_checksum(b"Y\n\r"), None);
        assert_eq!(parse_checksum(b"Z0000G5B0#"), None);
        assert_eq!(parse_checksum(b"Z+000D5B0#"), None);
    }

    #[test]
    fn keeps_the_printable_line() {
        assert_eq!(line(b"nRF52840-QIAA\n\r"), "nRF52840-QIAA");
        assert_eq!(line(b""), "");
    }
}
