//! Reading a manifest and the files it names from disk.

use crate::output::Failure;
use nanoflash::Error;
use nanoflash::flash::ImageSource;
use nanoflash::manifest::Manifest;
use std::fs;
use std::path::{Path, PathBuf};

/// Reads the files of a manifest from `directory`, the directory of the manifest.
pub struct FileSource {
    /// The directory paths are relative to.
    pub directory: PathBuf,
}

impl ImageSource for FileSource {
    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        if path.starts_with("http://") || path.starts_with("https://") {
            return Err(format!(
                "URLs are only fetched by the web package; download {path} next to the manifest"
            ));
        }
        let full = self.directory.join(path);
        fs::read(&full).map_err(|error| format!("cannot read {}: {error}", full.display()))
    }
}

/// Reads and parses the manifest at `path`, and the source of the files it names.
pub fn load_manifest(path: &Path) -> Result<(Manifest, FileSource), Failure> {
    let text = fs::read_to_string(path)
        .map_err(|error| Failure::Input(format!("cannot read {}: {error}", path.display())))?;
    let manifest = Manifest::parse(&text).map_err(|error| {
        Failure::Error(Error::Manifest {
            error: error.clone(),
        })
    })?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((manifest, FileSource { directory }))
}
