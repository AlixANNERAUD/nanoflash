//! The SAM-BA backend: Arduino's SAM-BA bootloaders of the Nano 33 BLE (nRF52840) and of the
//! SAMD21 boards, over an ASCII monitor protocol.

pub(crate) mod command;
pub(crate) mod family;

pub use command::crc16;
