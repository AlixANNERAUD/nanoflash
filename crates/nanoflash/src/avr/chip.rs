// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! The AVR chips the bootloaders flash: what their signature says about the flash.

use crate::manifest::Protocol;

/// An AVR chip, found by the signature its bootloader reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chip {
    /// The name, as the datasheet and `boards.txt` spell it.
    pub name: &'static str,
    /// The three signature bytes, vendor first.
    pub signature: [u8; 3],
    /// The size of the flash, in bytes.
    pub flash_size: u64,
    /// The size of a flash page, in bytes: what a bootloader programs at a time.
    pub page_size: u64,
}

/// The chips of the supported boards: the Uno and the Nano, the Mega 2560, the Leonardo and the
/// Micro.
pub const CHIPS: [Chip; 3] = [
    Chip {
        name: "atmega328p",
        signature: [0x1E, 0x95, 0x0F],
        flash_size: 0x8000,
        page_size: 0x80,
    },
    Chip {
        name: "atmega2560",
        signature: [0x1E, 0x98, 0x01],
        flash_size: 0x4_0000,
        page_size: 0x100,
    },
    Chip {
        name: "atmega32u4",
        signature: [0x1E, 0x95, 0x87],
        flash_size: 0x8000,
        page_size: 0x80,
    },
];

/// The chip with this signature, if it is one of the supported chips.
pub fn from_signature(signature: [u8; 3]) -> Option<&'static Chip> {
    CHIPS.iter().find(|chip| chip.signature == signature)
}

/// The bytes at the top of the flash that the bootloader of a protocol may occupy. The size
/// cannot be read back: Optiboot (512 bytes) answers at 115200 baud, ATmegaBOOT (2 KiB) at the
/// slower rates; the wiring bootloader of the Mega 2560 takes 8 KiB and Caterina 4 KiB. The
/// bootloaders of the other protocols do not sit at the top of the flash.
pub(crate) fn bootloader_size(protocol: Protocol, baud_rate: u32) -> u64 {
    match protocol {
        Protocol::Stk500v1 if baud_rate == 115_200 => 0x200,
        Protocol::Stk500v1 => 0x800,
        Protocol::Stk500v2 => 0x2000,
        Protocol::Avr109 => 0x1000,
        Protocol::Espressif | Protocol::Samba => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flash::prepare::{address_limit, erase_unit};

    #[test]
    fn finds_chips_by_signature() {
        let chip = from_signature([0x1E, 0x95, 0x0F]).unwrap();
        assert_eq!(
            (chip.name, chip.flash_size, chip.page_size),
            ("atmega328p", 0x8000, 0x80)
        );
        let chip = from_signature([0x1E, 0x98, 0x01]).unwrap();
        assert_eq!(
            (chip.name, chip.flash_size, chip.page_size),
            ("atmega2560", 0x4_0000, 0x100)
        );
        assert_eq!(
            from_signature([0x1E, 0x95, 0x87]).unwrap().name,
            "atmega32u4"
        );
        assert_eq!(from_signature([0x1E, 0x95, 0x16]), None);
    }

    #[test]
    fn bootloader_sizes() {
        assert_eq!(bootloader_size(Protocol::Stk500v1, 115_200), 0x200);
        assert_eq!(bootloader_size(Protocol::Stk500v1, 57_600), 0x800);
        assert_eq!(bootloader_size(Protocol::Stk500v2, 115_200), 0x2000);
        assert_eq!(bootloader_size(Protocol::Avr109, 57_600), 0x1000);
        assert_eq!(bootloader_size(Protocol::Samba, 115_200), 0);
    }

    #[test]
    fn erase_units_cover_every_page() {
        let chip = |name| CHIPS.iter().find(|chip| chip.name == name).unwrap();
        // Images start on a page of the chip each protocol flashes.
        assert_eq!(erase_unit(Protocol::Stk500v1), chip("atmega328p").page_size);
        assert_eq!(erase_unit(Protocol::Stk500v2), chip("atmega2560").page_size);
        assert_eq!(erase_unit(Protocol::Avr109), chip("atmega32u4").page_size);
        assert_eq!(address_limit(Protocol::Stk500v2), 1 << 32);
    }
}
