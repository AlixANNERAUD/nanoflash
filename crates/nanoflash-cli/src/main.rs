//! The `nanoflash` command.

#![forbid(unsafe_code)]

mod device;
mod image;
mod layout;
mod output;
mod source;

use getargs_derive::{Error as ArgumentError, GetArgs, Subcommand, ValueEnum};
use output::Failure;
use std::ffi::OsString;
use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// Lay out flash and write firmware to microcontroller boards of any vendor.
#[derive(Debug, GetArgs)]
#[command(name = "nanoflash", version)]
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
    /// Write the images of a board to the device on a serial port.
    Flash {
        /// The board manifest (TOML or JSON).
        #[arg(long, value_name = "MANIFEST")]
        board: PathBuf,
        /// The serial port; by default the only one that suits the board.
        #[arg(long, conflicts_with = "replay")]
        port: Option<String>,
        /// The board of the manifest to flash, when the device cannot tell.
        #[arg(long)]
        board_identifier: Option<String>,
        /// Erase the whole flash of the chip before writing; on SAM-BA, all of it after the bootloader.
        #[arg(long)]
        erase_all: bool,
        /// Do not check what was written.
        #[arg(long)]
        no_verify: bool,
        /// The baud rate to flash at [default: 460800 through a UART bridge, 115200 on USB-Serial-JTAG; ignored on SAM-BA].
        #[arg(long)]
        baud_rate: Option<u32>,
        /// How to report progress on standard error; quiet with --json.
        #[arg(long, value_enum)]
        progress: Option<ProgressFormat>,
        /// Print one JSON document instead of sentences.
        #[arg(long)]
        json: bool,
        #[arg(long, hide)]
        record: Option<PathBuf>,
        #[arg(long, hide)]
        replay: Option<PathBuf>,
    },
    /// Connect to a device and tell what chip it is.
    Detect {
        /// The serial port.
        #[arg(long, conflicts_with = "replay", required_unless_present = "replay")]
        port: Option<String>,
        /// The protocol of the device, when its USB identity does not tell.
        #[arg(long, value_enum)]
        protocol: Option<ProtocolArgument>,
        /// How to report progress on standard error; quiet with --json.
        #[arg(long, value_enum)]
        progress: Option<ProgressFormat>,
        /// Print one JSON document instead of sentences.
        #[arg(long)]
        json: bool,
        #[arg(long, hide)]
        record: Option<PathBuf>,
        #[arg(long, hide)]
        replay: Option<PathBuf>,
    },
    /// Work with flash images.
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ImageCommand {
    /// Merge the images of a board into one flash image.
    Build {
        /// The board manifest (TOML or JSON).
        #[arg(long, value_name = "MANIFEST")]
        board: PathBuf,
        /// The board of the manifest, when it has several.
        #[arg(long)]
        board_identifier: Option<String>,
        /// The file to write.
        #[arg(long)]
        output: PathBuf,
        /// Pad the image to the flash size of the layout.
        #[arg(long)]
        fill: bool,
        /// Print one JSON document instead of a sentence.
        #[arg(long)]
        json: bool,
    },
}

/// How progress is reported on standard error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProgressFormat {
    /// One line per stage.
    Human,
    /// One JSON object per event.
    Json,
    /// Nothing.
    Quiet,
}

/// The protocols `detect` can be told to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProtocolArgument {
    /// The Espressif ROM loader.
    Espressif,
    /// Arduino's SAM-BA bootloaders (Nano 33 BLE, SAMD21 boards).
    Samba,
}

#[derive(Debug, Subcommand)]
enum LayoutCommand {
    /// Check a layout and print the offset, size and headroom of every partition.
    Check {
        /// The layout file.
        layout: PathBuf,
        /// An image written to a partition, as PARTITION=FILE (repeatable).
        #[arg(
            long = "image",
            value_name = "PARTITION=FILE",
            conflicts_with = "manifest"
        )]
        images: Vec<String>,
        /// A board manifest: check the images it writes instead.
        #[arg(long, value_name = "MANIFEST")]
        manifest: Option<PathBuf>,
        /// The board of the manifest, when several use the layout.
        #[arg(long, requires = "manifest")]
        board_identifier: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
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
    let raw: Vec<OsString> = std::env::args_os().collect();
    let arguments = match Arguments::from_args_os() {
        Ok(arguments) => arguments,
        Err(error) => return argument_error(error, &raw),
    };
    let (json, result) = run(arguments.command);
    match result {
        Ok(text) => emit(&mut io::stdout().lock(), &text, 0),
        Err(failure) if json => emit(&mut io::stdout().lock(), &failure.json(), failure.code()),
        Err(failure) => emit(&mut io::stderr().lock(), &failure.human(), failure.code()),
    }
}

/// Run a command: whether it was asked for JSON, and what it printed or why it failed.
fn run(command: Command) -> (bool, Result<String, Failure>) {
    match command {
        Command::Layout { command } => match command {
            LayoutCommand::Check {
                layout,
                images,
                manifest,
                board_identifier,
                json,
            } => {
                let choice = manifest.as_deref().map(|manifest| layout::ManifestChoice {
                    manifest,
                    board_identifier: board_identifier.as_deref(),
                });
                (json, layout::check(&layout, &images, choice, json))
            }
            LayoutCommand::Write {
                layout,
                format,
                output,
                json,
            } => (json, layout::write(&layout, format, &output, json)),
        },
        Command::Flash {
            board,
            port,
            board_identifier,
            erase_all,
            no_verify,
            baud_rate,
            progress,
            json,
            record,
            replay,
        } => (
            json,
            device::flash_command(&device::FlashArguments {
                board: &board,
                port: port.as_deref(),
                board_identifier: board_identifier.as_deref(),
                erase_all,
                no_verify,
                baud_rate,
                progress,
                json,
                record: record.as_deref(),
                replay: replay.as_deref(),
            }),
        ),
        Command::Detect {
            port,
            protocol,
            progress,
            json,
            record,
            replay,
        } => (
            json,
            device::detect_command(&device::DetectArguments {
                port: port.as_deref(),
                protocol,
                progress,
                json,
                record: record.as_deref(),
                replay: replay.as_deref(),
            }),
        ),
        Command::Image { command } => match command {
            ImageCommand::Build {
                board,
                board_identifier,
                output,
                fill,
                json,
            } => (
                json,
                image::build(&board, board_identifier.as_deref(), &output, fill, json),
            ),
        },
    }
}

/// Write `text` and return `code`; a closed pipe ends quietly with `code`.
fn emit(destination: &mut impl Write, text: &str, code: u8) -> ExitCode {
    match destination
        .write_all(text.as_bytes())
        .and_then(|()| destination.flush())
    {
        Err(error) if error.kind() != ErrorKind::BrokenPipe => ExitCode::from(1),
        _ => ExitCode::from(code),
    }
}

/// Report an argument error: help and version succeed, anything else exits 1, as JSON when
/// `--json` was given. The JSON message is the bare error, without the usage text.
fn argument_error(error: ArgumentError<'_>, raw: &[OsString]) -> ExitCode {
    if error.is_help() || error.is_version() {
        return emit(&mut io::stdout().lock(), &format!("{error}\n"), 0);
    }
    if raw.iter().any(|argument| argument == "--json") {
        let message = error.to_string().trim_end().to_string();
        emit(&mut io::stdout().lock(), &Failure::Input(message).json(), 1)
    } else {
        // Subcommand errors carry the usage of the top-level command, the most specific one.
        let report = error.with_usage(Arguments::USAGE);
        emit(&mut io::stderr().lock(), &format!("error: {report}\n"), 1)
    }
}
