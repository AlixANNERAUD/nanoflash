//! Choosing the board a device is and preparing the images its manifest flashes.

mod prepare;
mod select;

use alloc::string::String;
use alloc::vec::Vec;

pub use prepare::{Prepared, PreparedImage, Warning, image_build, prepare};
pub use select::{Selection, protocol_for_identity, select};

/// Reads the files a manifest names.
///
/// Paths are exactly as written in the manifest, relative to it or URLs; the source resolves them.
// Futures are not Send: browser APIs are single-threaded (docs/design.md, Constraints).
#[allow(async_fn_in_trait)]
pub trait ImageSource {
    /// The bytes of the file at `path`, or what went wrong.
    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String>;
}

/// Decides whether an image may be flashed, such as by checking a signature.
pub trait ImageVerifier {
    /// Accepts the image read from `path`, or says why not.
    fn verify(&mut self, path: &str, bytes: &[u8]) -> Result<(), String>;
}

impl ImageVerifier for () {
    fn verify(&mut self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Ok(())
    }
}
