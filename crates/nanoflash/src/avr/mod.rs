// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! The AVR backend: the STK500v1 (Optiboot, ATmegaBOOT), STK500v2 (the wiring bootloader of the
//! Mega 2560) and AVR109 (Caterina) bootloaders of Arduino's boards.

pub(crate) mod avr109;
pub(crate) mod chip;
pub(crate) mod stk500v1;
pub(crate) mod stk500v2;

pub use chip::{CHIPS, Chip};
