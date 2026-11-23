// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! The STK500v1 commands of Optiboot and ATmegaBOOT.

use alloc::format;
use alloc::string::String;
#[cfg(test)]
use alloc::vec::Vec;

use super::hexadecimal;

// The page commands are compiled for the tests only until the page loop programs and reads
// pages with them.

/// `STK_INSYNC`.
const IN_SYNC: u8 = 0x14;
/// `STK_NOSYNC`.
const NO_SYNC: u8 = 0x15;
/// `STK_OK`.
const OK: u8 = 0x10;
/// `CRC_EOP`, which ends every command.
const END: u8 = 0x20;
/// The memory type of flash, `F`.
#[cfg(test)]
const FLASH: u8 = 0x46;

/// `STK_GET_SYNC`.
pub(crate) const GET_SYNC: [u8; 2] = [0x30, END];
/// `STK_READ_SIGN`.
pub(crate) const READ_SIGNATURE: [u8; 2] = [0x75, END];
/// `STK_LEAVE_PROGMODE`.
#[cfg(test)]
pub(crate) const LEAVE: [u8; 2] = [0x51, END];

/// `STK_LOAD_ADDRESS` for a byte offset, which the protocol takes as a 16-bit word address;
/// `None` when the word does not fit.
#[cfg(test)]
pub(crate) fn load_address(offset: u64) -> Option<[u8; 4]> {
    let word = u16::try_from(offset / 2).ok()?;
    let [low, high] = word.to_le_bytes();
    Some([0x55, low, high, END])
}

/// `STK_PROG_PAGE` of flash. The length saturates at 65535: pages are a few hundred bytes.
#[cfg(test)]
pub(crate) fn program_page(data: &[u8]) -> Vec<u8> {
    let length = u16::try_from(data.len()).unwrap_or(u16::MAX);
    let [high, low] = length.to_be_bytes();
    let mut command = Vec::with_capacity(data.len().saturating_add(5));
    command.extend_from_slice(&[0x64, high, low, FLASH]);
    command.extend_from_slice(data);
    command.push(END);
    command
}

/// `STK_READ_PAGE` of flash.
#[cfg(test)]
pub(crate) fn read_page(length: u16) -> [u8; 5] {
    let [high, low] = length.to_be_bytes();
    [0x74, high, low, FLASH, END]
}

/// The payload of an answer: `14`, `payload` bytes, `10`.
pub(crate) fn answer(bytes: &[u8], payload: usize) -> Result<&[u8], String> {
    if let [IN_SYNC, data @ .., OK] = bytes {
        if data.len() == payload {
            return Ok(data);
        }
    }
    if bytes.first() == Some(&NO_SYNC) {
        return Err(String::from(
            "the bootloader lost synchronisation (STK_NOSYNC)",
        ));
    }
    Err(format!(
        "expected 14 <{payload} bytes> 10, got {}",
        hexadecimal(bytes)
    ))
}

/// Whether the bytes end with the answer to `STK_GET_SYNC`, `14 10`.
pub(crate) fn synced(bytes: &[u8]) -> bool {
    bytes.ends_with(&[IN_SYNC, OK])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stk500v1_commands() {
        assert_eq!(GET_SYNC, [0x30, 0x20]);
        assert_eq!(READ_SIGNATURE, [0x75, 0x20]);
        assert_eq!(LEAVE, [0x51, 0x20]);
        assert_eq!(load_address(0x80), Some([0x55, 0x40, 0x00, 0x20]));
        assert_eq!(load_address(0x1_0000), Some([0x55, 0x00, 0x80, 0x20]));
        assert_eq!(load_address(0x2_0000), None);
        let page = program_page(&[0xAA; 128]);
        assert_eq!(page[..4], [0x64, 0x00, 0x80, 0x46]);
        assert_eq!(page.last(), Some(&0x20));
        assert_eq!(page.len(), 133);
        assert_eq!(read_page(128), [0x74, 0x00, 0x80, 0x46, 0x20]);
    }

    #[test]
    fn stk500v1_answers() {
        assert_eq!(
            answer(&[0x14, 0x1E, 0x95, 0x0F, 0x10], 3),
            Ok(&[0x1E, 0x95, 0x0F][..])
        );
        assert_eq!(answer(&[0x14, 0x10], 0), Ok(&[][..]));
        assert!(answer(&[0x15], 0).unwrap_err().contains("STK_NOSYNC"));
        let message = answer(&[0x14, 0x11], 0).unwrap_err();
        assert!(message.contains("got 14 11"), "{message}");
        assert!(answer(&[0x14, 0x10], 1).is_err());
        assert!(answer(&[], 0).is_err());
        assert!(synced(&[0x14, 0x10, 0x14, 0x10]));
        assert!(synced(&[0x00, 0x14, 0x10]));
        assert!(!synced(&[0x14]));
    }
}
