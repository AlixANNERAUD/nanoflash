//! `nanoflash image build`: merging the images of a board into one flash image.

use crate::output::{self, Failure};
use crate::source;
use nanoflash::flash::image_build_for_flash;
use nanoflash::quantity;
use std::fs;
use std::path::Path;

pub fn build(
    manifest: &Path,
    board_identifier: Option<&str>,
    destination: &Path,
    fill: bool,
    flash_size: Option<&str>,
    json: bool,
) -> Result<String, Failure> {
    let flash_size = flash_size
        .map(|text| {
            quantity::parse(text)
                .map_err(|error| Failure::Input(format!("bad --flash-size: {error}")))
        })
        .transpose()?;
    let (manifest, mut source) = source::load_manifest(manifest)?;
    let bytes = pollster::block_on(image_build_for_flash(
        &manifest,
        board_identifier,
        &mut source,
        &mut (),
        fill,
        flash_size,
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
