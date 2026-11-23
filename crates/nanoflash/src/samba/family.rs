// Flash geometry, buffer addresses, device identifiers and command formats come from observing bossac (arduino/BOSSA, BSD-3-Clause, behaviour reference only) and the ArduinoCore-samd bootloader (LGPL-2.1, protocol reference only); no code is taken from either.
//! The boards behind SAM-BA bootloaders and what differs between them.

/// What differs between the boards behind SAM-BA bootloaders.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Family {
    /// The chip name reported to callers.
    pub chip: &'static str,
    /// Size of the flash in bytes.
    pub flash_size: u64,
    /// Flash offset where the application starts: below it the bootloader lives.
    pub application_start: u64,
    /// Whether command addresses count from `application_start` instead of the flash start.
    pub relative_addresses: bool,
    /// The RAM address `S` and `Y` use for the write buffer.
    pub buffer: u32,
    /// The size the last chunk of an image is padded to with `0xFF`.
    pub write_unit: u64,
    /// Whether `X` erases from its address to the end of flash (otherwise `Y` erases the pages
    /// it programs and `X` erases nothing).
    pub erase_to_end: bool,
}

/// The nRF52840 behind Arduino's mbed bootloader (Arduino Nano 33 BLE).
const NRF52840: Family = Family {
    chip: "nrf52840",
    flash_size: 0x10_0000,
    application_start: 0x1_0000,
    relative_addresses: true,
    buffer: 0x34,
    write_unit: 0x1000,
    erase_to_end: false,
};

const fn samd21(chip: &'static str) -> Family {
    Family {
        chip,
        flash_size: 0x4_0000,
        application_start: 0x2000,
        relative_addresses: false,
        buffer: 0x2000_4034,
        write_unit: 0x40,
        erase_to_end: true,
    }
}

const SAMD21J18A: Family = samd21("samd21j18a");
const SAMD21G18A: Family = samd21("samd21g18a");
const SAMD21E18A: Family = samd21("samd21e18a");

/// Where SAMD21 chips store their device identifier.
pub(crate) const DEVICE_IDENTIFIER_ADDRESS: u32 = 0x4100_2018;
/// The Cortex-M application interrupt and reset control register.
pub(crate) const AIRCR: u32 = 0xE000_ED0C;
/// The value that requests a system reset through `AIRCR` (key and `SYSRESETREQ`).
pub(crate) const SYSTEM_RESET: u32 = 0x05FA_0004;
/// The size of the RAM buffer, and of each chunk written and checked.
pub(crate) const BUFFER_SIZE: usize = 4096;

/// The family of an identification line (`I`), or `None` when it is not an nRF52840.
pub(crate) fn from_identification(reply: &str) -> Option<&'static Family> {
    reply.starts_with("nRF52840").then_some(&NRF52840)
}

/// The family of a SAMD21 device identifier (the word at [`DEVICE_IDENTIFIER_ADDRESS`]), ignoring
/// the revision and die number bits.
pub(crate) fn from_device_identifier(value: u32) -> Option<&'static Family> {
    match value & 0xFFFF_00FF {
        0x1001_0000 => Some(&SAMD21J18A),
        0x1001_0005 | 0x1001_000F => Some(&SAMD21G18A),
        0x1001_000A => Some(&SAMD21E18A),
        _ => None,
    }
}

impl Family {
    /// The address to put in a command for the flash offset `offset`, or `None` when it is below
    /// the application start (relative addresses) or does not fit 32 bits.
    pub(crate) fn command_address(&self, offset: u64) -> Option<u32> {
        let address = if self.relative_addresses {
            offset.checked_sub(self.application_start)?
        } else {
            offset
        };
        u32::try_from(address).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_families() {
        assert_eq!(
            from_identification("nRF52840-QIAA").map(|f| f.chip),
            Some("nrf52840")
        );
        assert!(from_identification("R7FA4M1AB").is_none());
        let chip = |value| from_device_identifier(value).map(|f| f.chip);
        assert_eq!(chip(0x1001_0305), Some("samd21g18a"));
        assert_eq!(chip(0x1001_0300), Some("samd21j18a"));
        assert_eq!(chip(0x1001_000A), Some("samd21e18a"));
        assert_eq!(chip(0x1001_0002), None);
        let nrf = from_identification("nRF52840").unwrap();
        assert_eq!(nrf.command_address(0x10000), Some(0));
        assert_eq!(nrf.command_address(0x11000), Some(0x1000));
        assert_eq!(nrf.command_address(0xF000), None);
        assert_eq!(nrf.command_address(0x1_0000_0000 + 0x10000), None);
        let samd = from_device_identifier(0x1001_0005).unwrap();
        assert_eq!(samd.command_address(0x2000), Some(0x2000));
        assert_eq!(samd.command_address(0x1_0000_0000), None);
    }
}
