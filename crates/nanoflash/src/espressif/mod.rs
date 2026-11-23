//! The Espressif ROM loader protocol: SLIP framing, command encoding and parsing, and the chip table.

// The backend that consumes the crate-private items lands after the protocol layer.
#[allow(dead_code)]
pub(crate) mod chip;
#[allow(dead_code)]
pub(crate) mod command;
#[allow(dead_code)]
pub(crate) mod slip;

pub use chip::Chip;

#[cfg(test)]
mod tests;
