//! `nanoflash flash` and `nanoflash detect`: talking to a board, or replaying a transcript.

use crate::output::{self, Failure};
use crate::source::{self, FileSource};
use crate::{ProgressFormat, ProtocolArgument};
use nanoflash::Error;
use nanoflash::backend::{BackendOptions, ChipInformation, Progress, ProgressEvent, Stage};
use nanoflash::flash::{FlashOptions, FlashReport, Hooks, detect, flash};
use nanoflash::manifest::{Manifest, Protocol};
use nanoflash::native::{NativePortProvider, NativeSerialPort, PortInformation, list_ports};
use nanoflash::quantity;
use nanoflash::testing::{Transcript, record, replay};
use nanoflash::transport::{DeviceFilter, PortProvider, TransportError};
use std::fs;
use std::io::Write;
use std::path::Path;

/// The baud rate a serial port is opened at: the one the ROM loader starts at. The backend
/// switches to its own baud rate once connected.
const OPEN_BAUD_RATE: u32 = 115_200;

/// Where the serial session comes from and goes to.
pub struct Connection<'a> {
    /// The port to open, else the only suitable one.
    pub port: Option<&'a str>,
    /// A transcript to replay instead of using a port.
    pub replay: Option<&'a Path>,
    /// A file to write the transcript of the session to.
    pub record: Option<&'a Path>,
    /// The USB identities the ports are looked up by when no port is given.
    pub filters: Vec<DeviceFilter>,
}

/// What to do once a port is open.
trait Operation {
    type Output;

    async fn run<P: PortProvider>(
        self,
        port: P::Port,
        provider: &mut P,
    ) -> Result<Self::Output, Error>;
}

/// Reports progress on standard error.
struct Reporter {
    format: ProgressFormat,
    last: Option<(Stage, u64)>,
}

impl Reporter {
    fn new(format: ProgressFormat) -> Reporter {
        Reporter { format, last: None }
    }
}

fn stderr_line(line: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.write_all(b"\n");
}

impl Progress for Reporter {
    fn report(&mut self, event: ProgressEvent) {
        match self.format {
            ProgressFormat::Quiet => {}
            ProgressFormat::Json => {
                if let Ok(line) = serde_json::to_string(&event) {
                    stderr_line(&line);
                }
            }
            ProgressFormat::Human => {
                // One line per stage change; erasing and writing also change with the offset.
                let key = match event.stage {
                    Stage::Erasing | Stage::Writing => (event.stage, event.offset),
                    _ => (event.stage, 0),
                };
                if self.last == Some(key) {
                    return;
                }
                self.last = Some(key);
                let line = match event.stage {
                    Stage::Connecting => "connecting".to_string(),
                    Stage::Erasing => format!(
                        "erasing {:#x}..{:#x}",
                        event.offset,
                        event.offset.saturating_add(event.total)
                    ),
                    Stage::Writing => format!("writing at {:#x}", event.offset),
                    Stage::Verifying => "verifying".to_string(),
                    Stage::Resetting => "resetting".to_string(),
                };
                stderr_line(&line);
            }
        }
    }

    fn message(&mut self, text: &str) {
        match self.format {
            ProgressFormat::Quiet => {}
            ProgressFormat::Json => stderr_line(&serde_json::json!({ "note": text }).to_string()),
            ProgressFormat::Human => stderr_line(&format!("note: {text}")),
        }
    }
}

fn input(message: String) -> Failure {
    Failure::Input(message)
}

/// The serial port to use: the one asked for, else the only port that matches `filters`, else
/// the only USB port.
fn choose_port(port: Option<&str>, filters: &[DeviceFilter]) -> Result<String, Failure> {
    if let Some(port) = port {
        return Ok(port.to_string());
    }
    let ports = list_ports().map_err(|error| Failure::Error(error.into()))?;
    let matching: Vec<&PortInformation> = ports
        .iter()
        .filter(|port| {
            port.identity
                .as_ref()
                .is_some_and(|identity| filters.iter().any(|filter| filter.matches(identity)))
        })
        .collect();
    let candidates = if matching.is_empty() {
        ports
            .iter()
            .filter(|port| port.identity.is_some())
            .collect()
    } else {
        matching
    };
    match candidates.as_slice() {
        [port] => Ok(port.path.clone()),
        [] => Err(input(
            "no serial port found: plug the board in, or pass --port".to_string(),
        )),
        several => {
            let paths: Vec<&str> = several.iter().map(|port| port.path.as_str()).collect();
            Err(input(format!(
                "several serial ports found ({}): pass --port",
                paths.join(", ")
            )))
        }
    }
}

fn transcript_error(path: &Path, error: impl std::fmt::Display) -> Failure {
    input(format!("{}: {error}", path.display()))
}

/// Runs `operation` over a port and its provider, recording the session when asked, and, for a
/// replay, checking that the whole transcript was used.
fn drive<P: PortProvider, O: Operation>(
    operation: O,
    port: P::Port,
    provider: P,
    record_to: Option<&Path>,
    finish: impl Fn(&P) -> Result<(), TransportError>,
) -> Result<O::Output, Failure> {
    let (outcome, session) = match record_to {
        None => {
            let mut provider = provider;
            let outcome = pollster::block_on(operation.run(port, &mut provider));
            (outcome, finish(&provider))
        }
        Some(destination) => {
            let (port, mut provider) = record(port, provider);
            let outcome = pollster::block_on(operation.run(port, &mut provider));
            let written = fs::write(destination, provider.transcript().to_string())
                .map_err(|error| input(format!("cannot write {}: {error}", destination.display())));
            let session = finish(provider.inner());
            // The transcript is written whether the command succeeded or not.
            if let Err(failure) = written {
                return outcome.map_err(Failure::Error).and(Err(failure));
            }
            (outcome, session)
        }
    };
    let value = outcome.map_err(Failure::Error)?;
    session.map_err(|error| Failure::Error(error.into()))?;
    Ok(value)
}

fn connect<O: Operation>(connection: &Connection<'_>, operation: O) -> Result<O::Output, Failure> {
    if let Some(file) = connection.replay {
        let text = fs::read_to_string(file)
            .map_err(|error| input(format!("cannot read {}: {error}", file.display())))?;
        let transcript = Transcript::parse(&text).map_err(|error| transcript_error(file, error))?;
        let (port, provider) = replay(transcript);
        return drive(operation, port, provider, connection.record, |provider| {
            provider.finish()
        });
    }
    let path = choose_port(connection.port, &connection.filters)?;
    let port = NativeSerialPort::open(&path, OPEN_BAUD_RATE)
        .map_err(|error| Failure::Error(error.into()))?;
    drive(
        operation,
        port,
        NativePortProvider,
        connection.record,
        |_| Ok(()),
    )
}

struct FlashOperation {
    manifest: Manifest,
    source: FileSource,
    options: FlashOptions,
    progress: ProgressFormat,
}

impl Operation for FlashOperation {
    type Output = FlashReport;

    async fn run<P: PortProvider>(
        mut self,
        port: P::Port,
        provider: &mut P,
    ) -> Result<FlashReport, Error> {
        let mut reporter = Reporter::new(self.progress);
        flash(
            &self.manifest,
            port,
            provider,
            Hooks {
                source: &mut self.source,
                verifier: &mut (),
                progress: &mut reporter,
            },
            &self.options,
        )
        .await
    }
}

struct DetectOperation {
    protocol: Option<Protocol>,
    options: BackendOptions,
    progress: ProgressFormat,
}

impl Operation for DetectOperation {
    type Output = ChipInformation;

    async fn run<P: PortProvider>(
        self,
        port: P::Port,
        provider: &mut P,
    ) -> Result<ChipInformation, Error> {
        let mut reporter = Reporter::new(self.progress);
        detect(port, provider, self.protocol, &self.options, &mut reporter).await
    }
}

/// The progress format: the one asked for, else none with `--json` and human otherwise.
fn progress_format(asked: Option<ProgressFormat>, json: bool) -> ProgressFormat {
    asked.unwrap_or(if json {
        ProgressFormat::Quiet
    } else {
        ProgressFormat::Human
    })
}

pub struct FlashArguments<'a> {
    pub board: &'a Path,
    pub port: Option<&'a str>,
    pub board_identifier: Option<&'a str>,
    pub erase_all: bool,
    pub no_verify: bool,
    pub baud_rate: Option<u32>,
    pub progress: Option<ProgressFormat>,
    pub json: bool,
    pub record: Option<&'a Path>,
    pub replay: Option<&'a Path>,
}

pub fn flash_command(arguments: &FlashArguments<'_>) -> Result<String, Failure> {
    let (manifest, source) = source::load_manifest(arguments.board)?;
    let filters = manifest
        .boards
        .iter()
        .filter(|board| {
            arguments
                .board_identifier
                .is_none_or(|identifier| board.identifier == identifier)
        })
        .flat_map(|board| {
            board
                .application_devices
                .iter()
                .chain(&board.bootloader_devices)
        })
        .cloned()
        .collect();
    let connection = Connection {
        port: arguments.port,
        replay: arguments.replay,
        record: arguments.record,
        filters,
    };
    let report = connect(
        &connection,
        FlashOperation {
            manifest,
            source,
            options: FlashOptions {
                board_identifier: arguments.board_identifier.map(String::from),
                erase_all: arguments.erase_all,
                verify: !arguments.no_verify,
                backend: BackendOptions {
                    baud_rate: arguments.baud_rate,
                },
            },
            progress: progress_format(arguments.progress, arguments.json),
        },
    )?;
    if arguments.json {
        return Ok(output::flash_json(&report));
    }
    output::print_warnings(&report.warnings);
    Ok(flash_text(&report))
}

fn flash_text(report: &FlashReport) -> String {
    let size = report
        .chip
        .flash_size
        .map_or_else(|| "unknown flash size".to_string(), quantity::format);
    let mut text = format!("flashed {} ({}, {size})\n", report.board, report.chip.chip);
    for image in &report.images {
        text.push_str(&format!(
            "  {:#x}  {}  {}\n",
            image.offset,
            image.label,
            quantity::format(image.size)
        ));
    }
    text.push_str(if report.verified {
        "verified\n"
    } else {
        "not verified (--no-verify)\n"
    });
    text
}

pub struct DetectArguments<'a> {
    pub port: Option<&'a str>,
    pub protocol: Option<ProtocolArgument>,
    pub baud_rate: Option<u32>,
    pub progress: Option<ProgressFormat>,
    pub json: bool,
    pub record: Option<&'a Path>,
    pub replay: Option<&'a Path>,
}

pub fn detect_command(arguments: &DetectArguments<'_>) -> Result<String, Failure> {
    let connection = Connection {
        port: arguments.port,
        replay: arguments.replay,
        record: arguments.record,
        filters: Vec::new(),
    };
    let chip = connect(
        &connection,
        DetectOperation {
            protocol: arguments.protocol.map(|protocol| match protocol {
                ProtocolArgument::Espressif => Protocol::Espressif,
            }),
            options: BackendOptions {
                baud_rate: arguments.baud_rate,
            },
            progress: progress_format(arguments.progress, arguments.json),
        },
    )?;
    if arguments.json {
        return Ok(output::detect_json(&chip));
    }
    let flash = chip
        .flash_size
        .map_or_else(|| "unknown".to_string(), quantity::format);
    let usb = chip.usb.as_ref().map_or_else(
        || "none".to_string(),
        |usb| format!("{:04x}:{:04x}", usb.vendor, usb.product),
    );
    Ok(format!(
        "protocol {}\nchip {}\nflash {flash}\nusb {usb}\nsecure download mode {}\n",
        chip.protocol,
        chip.chip,
        if chip.secure_download_mode {
            "yes"
        } else {
            "no"
        }
    ))
}
