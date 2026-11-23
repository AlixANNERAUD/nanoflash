// Chip identifiers, detection values, SPI and watchdog register addresses and flash size identifiers are copied from espflash 4.7.0 (https://github.com/esp-rs/espflash, src/target/mod.rs, src/flasher/mod.rs and src/flasher/stubs.rs), Copyright (c) 2022-2025 The Espflash Project Developers, licensed MIT OR Apache-2.0. The ESP8266 magic value comes from esptool-js (Apache-2.0).

//! The Espressif chip table.

/// The key that unlocks the RTC watchdog registers.
pub(crate) const WATCHDOG_KEY: u32 = 0x50D8_3AA1;
/// The register every ROM exposes a chip-dependent magic value at.
pub(crate) const MAGIC_ADDRESS: u32 = 0x4000_1000;
/// The magic value of an ESP8266, which this crate recognises but does not support.
pub(crate) const ESP8266_MAGIC: u32 = 0xFFF0_C101;

/// An Espressif chip family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    Esp32,
    Esp32S2,
    Esp32S3,
    Esp32C2,
    Esp32C3,
    Esp32C5,
    Esp32C6,
    Esp32H2,
    Esp32P4,
}

/// The addresses of the SPI registers used to read the flash identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpiRegisters {
    pub command: u32,
    pub user: u32,
    pub user2: u32,
    pub miso_length: u32,
    pub data: u32,
}

/// The addresses of the RTC watchdog registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Watchdog {
    pub write_protect: u32,
    pub configuration: u32,
}

impl Chip {
    /// The lowercase name with dashes, such as `esp32-s3`.
    pub const fn name(self) -> &'static str {
        match self {
            Chip::Esp32 => "esp32",
            Chip::Esp32S2 => "esp32-s2",
            Chip::Esp32S3 => "esp32-s3",
            Chip::Esp32C2 => "esp32-c2",
            Chip::Esp32C3 => "esp32-c3",
            Chip::Esp32C5 => "esp32-c5",
            Chip::Esp32C6 => "esp32-c6",
            Chip::Esp32H2 => "esp32-h2",
            Chip::Esp32P4 => "esp32-p4",
        }
    }

    /// The chip for a `GET_SECURITY_INFO` chip identifier.
    pub fn from_chip_id(id: u32) -> Option<Chip> {
        match id {
            0 => Some(Chip::Esp32),
            2 => Some(Chip::Esp32S2),
            9 => Some(Chip::Esp32S3),
            12 => Some(Chip::Esp32C2),
            5 => Some(Chip::Esp32C3),
            23 => Some(Chip::Esp32C5),
            13 => Some(Chip::Esp32C6),
            16 => Some(Chip::Esp32H2),
            18 => Some(Chip::Esp32P4),
            _ => None,
        }
    }

    /// The chip for the magic value read at [`MAGIC_ADDRESS`].
    pub fn from_magic(value: u32) -> Option<Chip> {
        match value {
            0x00F0_1D83 => Some(Chip::Esp32),
            0x0000_07C6 => Some(Chip::Esp32S2),
            0x0000_0009 => Some(Chip::Esp32S3),
            0x6F51_306F | 0x7C41_A06F => Some(Chip::Esp32C2),
            0x6921_506F | 0x1B31_506F | 0x4881_606F | 0x4361_606F => Some(Chip::Esp32C3),
            0x1101_406F | 0x63E1_406F | 0x5FD1_406F => Some(Chip::Esp32C5),
            0x2CE0_806F => Some(Chip::Esp32C6),
            0xD7B7_3E80 => Some(Chip::Esp32H2),
            _ => None,
        }
    }

    /// Whether `FLASH_BEGIN` and `FLASH_DEFL_BEGIN` carry a fifth (encryption) word.
    // Writing, which comes with the `Backend` implementation, uses it.
    #[allow(dead_code)]
    pub(crate) const fn has_encryption_word(self) -> bool {
        !matches!(self, Chip::Esp32)
    }

    pub(crate) const fn spi_registers(self) -> SpiRegisters {
        let base = match self {
            Chip::Esp32 => 0x3FF4_2000,
            Chip::Esp32S2 => 0x3F40_2000,
            Chip::Esp32S3 | Chip::Esp32C2 | Chip::Esp32C3 => 0x6000_2000,
            Chip::Esp32C5 | Chip::Esp32C6 | Chip::Esp32H2 => 0x6000_3000,
            Chip::Esp32P4 => 0x5008_D000,
        };
        match self {
            Chip::Esp32 => SpiRegisters {
                command: base,
                user: base + 0x1C,
                user2: base + 0x24,
                miso_length: base + 0x2C,
                data: base + 0x80,
            },
            _ => SpiRegisters {
                command: base,
                user: base + 0x18,
                user2: base + 0x20,
                miso_length: base + 0x28,
                data: base + 0x58,
            },
        }
    }

    pub(crate) const fn watchdog(self) -> Option<Watchdog> {
        match self {
            Chip::Esp32S2 => Some(Watchdog {
                write_protect: 0x3F40_80AC,
                configuration: 0x3F40_8094,
            }),
            Chip::Esp32S3 => Some(Watchdog {
                write_protect: 0x6000_80B0,
                configuration: 0x6000_8098,
            }),
            Chip::Esp32C3 => Some(Watchdog {
                write_protect: 0x6000_80A8,
                configuration: 0x6000_8090,
            }),
            Chip::Esp32C5 => Some(Watchdog {
                write_protect: 0x600B_1C18,
                configuration: 0x600B_1C00,
            }),
            Chip::Esp32P4 => Some(Watchdog {
                write_protect: 0x5011_6018,
                configuration: 0x5011_6000,
            }),
            Chip::Esp32 | Chip::Esp32C2 | Chip::Esp32C6 | Chip::Esp32H2 => None,
        }
    }
}

/// The flash size in bytes for byte 2 of the JEDEC identifier.
pub(crate) fn flash_size(size_identifier: u8) -> Option<u64> {
    const KIBIBYTE: u64 = 1024;
    let kibibytes = match size_identifier {
        0x12 | 0x32 => 256,
        0x13 | 0x33 => 512,
        0x14 | 0x34 => 1024,
        0x15 | 0x35 => 2 * 1024,
        0x16 | 0x36 => 4 * 1024,
        0x17 | 0x37 => 8 * 1024,
        0x18 | 0x38 => 16 * 1024,
        0x19 | 0x39 => 32 * 1024,
        0x1A | 0x3A | 0x20 => 64 * 1024,
        0x1B | 0x21 => 128 * 1024,
        0x1C | 0x22 => 256 * 1024,
        _ => return None,
    };
    Some(kibibytes * KIBIBYTE)
}
