//! What the command prints: the human table, the errors and the JSON documents.

use nanoflash::backend::ChipInformation;
use nanoflash::flash::{FlashReport, Warning};
use nanoflash::layout::{LayoutError, PartitionReport, Report, Role};
use nanoflash::quantity;
use nanoflash::table::TableError;
use nanoflash::{Error, ErrorClass};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Write;

/// Everything that makes a command fail.
#[derive(Debug)]
pub enum Failure {
    /// A file, an argument or a layout file that cannot be read.
    Input(String),
    /// Problems found in the layout or in the images.
    Layout(Vec<LayoutError>),
    /// A layout the chosen table writer cannot express.
    Table(TableError),
    /// A failure of connecting, preparing or flashing.
    Error(Error),
}

impl Failure {
    /// The exit code: 1 for input, 2 for the device, 3 for a verification failure.
    pub fn code(&self) -> u8 {
        match self {
            Failure::Error(error) => match error.class() {
                ErrorClass::Input => 1,
                ErrorClass::Device => 2,
                ErrorClass::Verification => 3,
            },
            _ => 1,
        }
    }

    /// The errors as one line each.
    fn messages(&self) -> Vec<String> {
        match self {
            Failure::Input(message) => vec![message.clone()],
            Failure::Layout(errors) => errors.iter().map(ToString::to_string).collect(),
            Failure::Table(error) => vec![error.to_string()],
            Failure::Error(error) => error.to_string().lines().map(String::from).collect(),
        }
    }

    /// The errors as JSON objects, each with its `message`.
    fn objects(&self) -> Vec<Value> {
        match self {
            Failure::Input(message) => {
                vec![json!({"kind": "input", "class": "input", "message": message})]
            }
            Failure::Layout(errors) => errors
                .iter()
                .map(|error| with_message(error, error, ErrorClass::Input))
                .collect(),
            Failure::Table(error) => vec![with_message(error, error, ErrorClass::Input)],
            Failure::Error(error) => vec![with_message(error, error, error.class())],
        }
    }

    /// The text for standard error.
    pub fn human(&self) -> String {
        let messages = self.messages();
        let mut text = String::new();
        for message in &messages {
            let _ = writeln!(text, "error: {message}");
        }
        if matches!(self, Failure::Layout(_) | Failure::Table(_)) {
            let _ = writeln!(text, "error: the layout has {} problem(s)", messages.len());
        }
        text
    }

    /// The JSON document for standard output.
    pub fn json(&self) -> String {
        render(&json!({"ok": false, "errors": self.objects()}))
    }
}

/// Serialize an error and add its `message` and `class`.
fn with_message(
    error: &impl Serialize,
    display: &impl std::fmt::Display,
    class: ErrorClass,
) -> Value {
    let mut value = serde_json::to_value(error).unwrap_or_else(|_| json!({"kind": "unknown"}));
    if let Value::Object(map) = &mut value {
        map.insert("message".into(), Value::String(display.to_string()));
        map.insert(
            "class".into(),
            serde_json::to_value(class).unwrap_or(Value::Null),
        );
    }
    value
}

fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// The JSON document of a successful `check`, with the warnings of a manifest.
pub fn report_json(report: &Report, warnings: Option<&[Warning]>) -> String {
    match warnings {
        None => render(&json!({"ok": true, "report": report})),
        Some(warnings) => {
            let warnings: Vec<Value> = warnings.iter().map(warning_json).collect();
            render(&json!({"ok": true, "report": report, "warnings": warnings}))
        }
    }
}

fn warning_json(warning: &Warning) -> Value {
    let mut value = serde_json::to_value(warning).unwrap_or_else(|_| json!({"kind": "unknown"}));
    if let Value::Object(map) = &mut value {
        map.insert("message".into(), Value::String(warning.to_string()));
    }
    value
}

/// Print the warnings, one line each, on standard error.
pub fn print_warnings(warnings: &[Warning]) {
    let mut stderr = std::io::stderr().lock();
    for warning in warnings {
        let line = format!("warning: {warning}\n");
        let _ = std::io::Write::write_all(&mut stderr, line.as_bytes());
    }
}

/// The JSON document of a successful `flash`, each warning with its `message`.
pub fn flash_json(report: &FlashReport) -> String {
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    if let Some(Value::Array(warnings)) = value.get_mut("warnings") {
        *warnings = report.warnings.iter().map(warning_json).collect();
    }
    render(&json!({"ok": true, "report": value}))
}

/// The JSON document of a successful `detect`.
pub fn detect_json(chip: &ChipInformation) -> String {
    render(&json!({"ok": true, "chip": chip}))
}

/// The JSON document of a successful `write`.
pub fn written_json(output: &str, bytes: usize) -> String {
    render(&json!({"ok": true, "output": output, "bytes": bytes}))
}

fn role_name(role: Role) -> String {
    match role {
        Role::Bootloader => "bootloader".into(),
        Role::PartitionTable => "partition-table".into(),
        Role::Firmware => "firmware".into(),
        Role::Update { slot } => format!("update {slot}"),
        Role::Storage => "storage".into(),
        Role::CrashRecord => "crash-record".into(),
        Role::Custom => "custom".into(),
    }
}

fn headroom(entry: &PartitionReport) -> String {
    match (entry.headroom, entry.headroom_percent) {
        (Some(bytes), Some(percent)) => format!("{} ({percent}%)", quantity::format(bytes)),
        _ => "-".into(),
    }
}

/// The table printed by `check`.
pub fn report_table(report: &Report) -> String {
    let mut text = format!(
        "flash {} ({:#x}), unallocated {}\n",
        quantity::format(report.flash_size),
        report.flash_size,
        quantity::format(report.unallocated)
    );
    let mut rows =
        vec![["name", "role", "offset", "end", "size", "image", "headroom"].map(String::from)];
    for entry in &report.partitions {
        rows.push([
            entry.name.clone(),
            role_name(entry.role),
            format!("{:#x}", entry.offset),
            format!("{:#x}", entry.end),
            quantity::format(entry.size),
            entry
                .image_size
                .map_or_else(|| "-".into(), quantity::format),
            headroom(entry),
        ]);
    }
    let mut widths = [0usize; 7];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in &rows {
        let line = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        let _ = writeln!(text, "{}", line.trim_end());
    }
    text
}
