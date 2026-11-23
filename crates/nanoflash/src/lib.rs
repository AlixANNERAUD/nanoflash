//! Lay out flash and write firmware to microcontroller boards of any vendor, through one API.
//!
//! One layout file per board gives the flash size and the named partitions with a role;
//! nanoflash computes the offsets, checks alignment, overlap and size, and reports the headroom
//! of every partition.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(feature = "espressif")]
pub mod espressif;
pub mod layout;
pub mod manifest;
#[cfg(feature = "native")]
pub mod native;
pub mod quantity;
#[cfg(any(
    feature = "table-espressif",
    feature = "table-mbr",
    feature = "table-linker"
))]
pub mod table;
#[cfg(feature = "testing")]
pub mod testing;
pub mod transport;

#[cfg(feature = "build")]
pub mod build;

#[cfg(any(test, feature = "build", feature = "native"))]
extern crate std;
