//! `nanoflash image build`: merging the images of a board into one flash image.

use crate::output::{self, Failure};
use crate::source;
use nanoflash::flash::image_build;
use std::fs;
use std::path::Path;

pub fn build(
    manifest: &Path,
    board_identifier: Option<&str>,
    destination: &Path,
    fill: bool,
    json: bool,
) -> Result<String, Failure> {
    let (manifest, mut source) = source::load_manifest(manifest)?;
    let bytes = pollster::block_on(image_build(
        &manifest,
        board_identifier,
        &mut source,
        &mut (),
        fill,
    ))
    .map_err(Failure::Error)?;
    fs::write(destination, &bytes).map_err(|error| {
        Failure::Input(format!("cannot write {}: {error}", destination.display()))
    })?;
    let file = destination.display().to_string();
    Ok(if json {
        output::written_json(&file, bytes.len())
    } else {
        format!("wrote {} bytes to {file}\n", bytes.len())
    })
}
