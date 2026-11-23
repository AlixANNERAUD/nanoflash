//! Generate the constants and the linker script of a firmware from its layout file, in `build.rs`.
//!
//! ```no_run
//! nanoflash::build::generate("layout.toml").unwrap();
//! ```

use crate::layout::{Layout, LayoutErrors, ParseError};
use crate::table::TableError;
use crate::table::linker::{memory_regions, rust_constants};
use alloc::string::String;
use core::fmt;
use std::path::{Path, PathBuf};

/// The file of Rust constants, written to the output directory.
const CONSTANTS_FILE: &str = "nanoflash_layout.rs";
/// The file of linker regions, written to the output directory.
const LINKER_FILE: &str = "nanoflash_memory.x";

/// The files written by [`generate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// The Rust constants, to `include!` from the firmware.
    pub constants: PathBuf,
    /// The linker script that declares the memory regions.
    pub linker_script: PathBuf,
}

/// Why the files could not be generated.
///
/// `Debug` prints the same text as `Display`, so a build script that unwraps the error, or returns
/// it from `main`, shows the message of `nanoflash layout check`.
pub enum BuildError {
    /// A file could not be read or written.
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The cause.
        error: std::io::Error,
    },
    /// The layout file is not valid.
    Parse(ParseError),
    /// The layout has errors.
    Layout(LayoutErrors),
    /// A writer cannot express the layout.
    Table(TableError),
    /// Cargo did not set `OUT_DIR`, so this was not called from a build script.
    MissingOutputDirectory,
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Io { path, error } => {
                write!(formatter, "cannot access {}: {error}", path.display())
            }
            BuildError::Parse(error) => write!(formatter, "cannot read the layout: {error}"),
            BuildError::Layout(error) => write!(formatter, "{error}"),
            BuildError::Table(error) => write!(formatter, "{error}"),
            BuildError::MissingOutputDirectory => formatter.write_str(
                "the OUT_DIR environment variable is not set, call this from a build script",
            ),
        }
    }
}

impl fmt::Debug for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for BuildError {}

/// Generate the files in `OUT_DIR` and tell Cargo to rebuild when the layout changes and where
/// to find the linker script.
pub fn generate(layout_path: impl AsRef<Path>) -> Result<Generated, BuildError> {
    let layout_path = layout_path.as_ref();
    let output_directory = std::env::var_os("OUT_DIR").ok_or(BuildError::MissingOutputDirectory)?;
    let output_directory = PathBuf::from(output_directory);
    let generated = generate_into(layout_path, &output_directory)?;
    std::println!("cargo:rerun-if-changed={}", layout_path.display());
    std::println!("cargo:rustc-link-search={}", output_directory.display());
    Ok(generated)
}

/// Generate `nanoflash_layout.rs` and `nanoflash_memory.x` in a directory.
pub fn generate_into(layout_path: &Path, output_directory: &Path) -> Result<Generated, BuildError> {
    let text = std::fs::read_to_string(layout_path).map_err(|error| BuildError::Io {
        path: layout_path.into(),
        error,
    })?;
    let resolved = Layout::from_toml(&text)
        .map_err(BuildError::Parse)?
        .resolve()
        .map_err(BuildError::Layout)?;
    let constants: String = rust_constants(&resolved).map_err(BuildError::Table)?;
    let regions: String = memory_regions(&resolved).map_err(BuildError::Table)?;

    let generated = Generated {
        constants: output_directory.join(CONSTANTS_FILE),
        linker_script: output_directory.join(LINKER_FILE),
    };
    for (path, contents) in [
        (&generated.constants, constants),
        (&generated.linker_script, regions),
    ] {
        std::fs::write(path, contents).map_err(|error| BuildError::Io {
            path: path.clone(),
            error,
        })?;
    }
    Ok(generated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(std::format!("nanoflash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/layouts")
            .join(name)
    }

    #[test]
    fn generate_into_writes_both_files() {
        let directory = scratch("both");
        let generated = generate_into(&fixture("nrf52840-arduino.toml"), &directory).unwrap();
        let resolved =
            Layout::from_toml(&fs::read_to_string(fixture("nrf52840-arduino.toml")).unwrap())
                .unwrap()
                .resolve()
                .unwrap();
        assert_eq!(
            fs::read_to_string(&generated.constants).unwrap(),
            rust_constants(&resolved).unwrap()
        );
        assert_eq!(
            fs::read_to_string(&generated.linker_script).unwrap(),
            memory_regions(&resolved).unwrap()
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn generate_reports_layout_errors() {
        let directory = scratch("overlap");
        let layout = directory.join("layout.toml");
        fs::write(
            &layout,
            "[flash]\nsize = \"1 MiB\"\nbase-address = 0x0\n\n\
             [[partition]]\nname = \"a\"\nrole = \"firmware\"\noffset = 0x0\nsize = 0x20000\n\n\
             [[partition]]\nname = \"b\"\nrole = \"storage\"\noffset = 0x10000\nsize = 0x10000\n",
        )
        .unwrap();
        let error = generate_into(&layout, &directory).unwrap_err();
        assert!(matches!(error, BuildError::Layout(_)));
        assert!(error.to_string().contains("overlap"), "{error}");
        let debug = std::format!("{error:?}");
        assert!(debug.contains("overlap") && debug.contains("0x"), "{debug}");
        assert_eq!(debug, error.to_string());
        fs::remove_dir_all(&directory).unwrap();
    }
}
