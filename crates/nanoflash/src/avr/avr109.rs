// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from
// observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the
// wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! The AVR109 commands of Caterina, the bootloader of the Leonardo and the Micro.

use alloc::vec::Vec;

/// The command that answers the software identifier, `CATERIN`.
pub(crate) const IDENTIFIER: u8 = b'S';
/// The command that answers the signature.
pub(crate) const SIGNATURE: u8 = b's';
/// The command that leaves the bootloader.
pub(crate) const EXIT: u8 = b'E';
/// The answer to the commands that return nothing else: set address, write block and exit.
pub(crate) const DONE: u8 = b'\r';
/// The answer to a command the bootloader does not know.
pub(crate) const UNKNOWN: u8 = b'?';
/// The memory type of flash, `F`.
const FLASH: u8 = b'F';

/// Set the address to a byte offset, which the protocol takes as a 16-bit word address; `None`
/// when the word does not fit.
pub(crate) fn set_address(offset: u64) -> Option<[u8; 3]> {
    let word = u16::try_from(offset / 2).ok()?;
    let [high, low] = word.to_be_bytes();
    Some([b'A', high, low])
}

/// Write a block of flash. The length saturates at 65535: blocks are a page long.
pub(crate) fn write_block(data: &[u8]) -> Vec<u8> {
    let length = u16::try_from(data.len()).unwrap_or(u16::MAX);
    let [high, low] = length.to_be_bytes();
    let mut command = Vec::with_capacity(data.len().saturating_add(4));
    command.extend_from_slice(&[b'B', high, low, FLASH]);
    command.extend_from_slice(data);
    command
}

/// Read a block of flash.
pub(crate) fn read_block(length: u16) -> [u8; 4] {
    let [high, low] = length.to_be_bytes();
    [b'g', high, low, FLASH]
}

/// The signature from the three bytes of the reply, which come in reverse.
pub(crate) fn signature(reply: [u8; 3]) -> [u8; 3] {
    let [a, b, c] = reply;
    [c, b, a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avr109_commands() {
        assert_eq!((IDENTIFIER, SIGNATURE, EXIT), (b'S', b's', b'E'));
        assert_eq!(set_address(0x80), Some([b'A', 0x00, 0x40]));
        assert_eq!(set_address(0x2_0000), None);
        let block = write_block(&[0; 128]);
        assert_eq!(block[..4], [b'B', 0x00, 0x80, b'F']);
        assert_eq!(block.len(), 132);
        assert_eq!(read_block(128), [b'g', 0x00, 0x80, b'F']);
        assert_eq!(signature([0x87, 0x95, 0x1E]), [0x1E, 0x95, 0x87]);
    }
}
