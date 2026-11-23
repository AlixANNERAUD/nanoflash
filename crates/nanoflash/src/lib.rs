//! Lay out flash and write firmware to microcontroller boards of any vendor, through one API.
//!
//! One layout file per board gives the flash size and the named partitions with a role;
//! nanoflash computes the offsets, checks alignment, overlap and size, and reports the headroom
//! of every partition.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod layout;
pub mod quantity;

#[cfg(test)]
extern crate std;
