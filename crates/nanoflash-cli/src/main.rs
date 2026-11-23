//! The `nanoflash` command.

#![forbid(unsafe_code)]

mod output;

use clap::{Parser, Subcommand, ValueEnum};
use nanoflash::layout::{ImageSize, Layout, LayoutErrors, ResolvedLayout};
use nanoflash::table::{TableError, espressif, linker, mbr};
use output::Failure;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Lay out flash and write firmware to microcontroller boards of any vendor.
#[derive(Debug, Parser)]
#[command(name = "nanoflash", version, about)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Work with a layout file.
    Layout {
        #[command(subcommand)]
        command: LayoutCommand,
    },
}

#[derive(Debug, Subcommand)]
enum LayoutCommand {
    /// Check a layout and print the offset, size and headroom of every partition.
    Check {
        /// The layout file.
        layout: PathBuf,
        /// An image written to a partition, as PARTITION=FILE (repeatable).
        #[arg(long = "image", value_name = "PARTITION=FILE")]
        images: Vec<String>,
        /// Print one JSON document instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Write a partition table, a linker script or constants for a layout.
    Write {
        /// The layout file.
        layout: PathBuf,
        /// What to write.
        #[arg(long, value_enum)]
        format: Format,
        /// The file to write.
        #[arg(long)]
        output: PathBuf,
        /// Print one JSON document instead of a sentence.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    /// The Espressif partition table (binary).
    Espressif,
    /// The master boot record (binary).
    Mbr,
    /// Linker memory regions (text).
    Linker,
    /// Rust constants for a build script (text).
    Constants,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Espressif => "espressif",
            Format::Mbr => "mbr",
            Format::Linker => "linker",
            Format::Constants => "constants",
        }
    }
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let Command::Layout { command } = arguments.command;
    let (json, result) = match command {
        LayoutCommand::Check {
            layout,
            images,
            json,
        } => (json, check(&layout, &images, json)),
        LayoutCommand::Write {
            layout,
            format,
            output,
            json,
        } => (json, write(&layout, format, &output, json)),
    };
    match result {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            if json {
                print!("{}", failure.json());
            } else {
                eprint!("{}", failure.human());
            }
            ExitCode::from(1)
        }
    }
}

fn load(path: &Path) -> Result<ResolvedLayout, Failure> {
    let text = fs::read_to_string(path)
        .map_err(|error| Failure::Input(format!("cannot read {}: {error}", path.display())))?;
    let layout = Layout::from_toml(&text)
        .map_err(|error| Failure::Input(format!("{}: {error}", path.display())))?;
    layout
        .resolve()
        .map_err(|LayoutErrors(errors)| Failure::Layout(errors))
}

fn image_size(argument: &str) -> Result<ImageSize, Failure> {
    let Some((partition, file)) = argument.split_once('=') else {
        return Err(Failure::Input(format!(
            "bad --image argument \"{argument}\": expected PARTITION=FILE"
        )));
    };
    if partition.is_empty() || file.is_empty() {
        return Err(Failure::Input(format!(
            "bad --image argument \"{argument}\": expected PARTITION=FILE"
        )));
    }
    let metadata = fs::metadata(file)
        .map_err(|error| Failure::Input(format!("cannot read image {file}: {error}")))?;
    Ok(ImageSize {
        partition: partition.to_string(),
        size: metadata.len(),
    })
}

fn check(layout: &Path, images: &[String], json: bool) -> Result<String, Failure> {
    let resolved = load(layout)?;
    let sizes = images
        .iter()
        .map(|argument| image_size(argument))
        .collect::<Result<Vec<_>, _>>()?;
    let report = resolved
        .report(&sizes)
        .map_err(|LayoutErrors(errors)| Failure::Layout(errors))?;
    Ok(if json {
        output::report_json(&report)
    } else {
        output::report_table(&report)
    })
}

fn write(layout: &Path, format: Format, destination: &Path, json: bool) -> Result<String, Failure> {
    let resolved = load(layout)?;
    let bytes: Vec<u8> = match format {
        Format::Espressif => espressif::write(&resolved),
        Format::Mbr => mbr::write(&resolved).map(|table| table.to_vec()),
        Format::Linker => linker::memory_regions(&resolved).map(String::into_bytes),
        Format::Constants => linker::rust_constants(&resolved).map(String::into_bytes),
    }
    .map_err(|error: TableError| Failure::Table(error))?;
    fs::write(destination, &bytes).map_err(|error| {
        Failure::Input(format!("cannot write {}: {error}", destination.display()))
    })?;
    let file = destination.display().to_string();
    Ok(if json {
        output::written_json(&file, bytes.len())
    } else {
        format!(
            "wrote {} table to {file} ({} bytes)\n",
            format.name(),
            bytes.len()
        )
    })
}
