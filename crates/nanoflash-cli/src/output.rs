//! What the command prints: the human table, the errors and the JSON documents.

use nanoflash::layout::{LayoutError, PartitionReport, Report, Role};
use nanoflash::quantity;
use nanoflash::table::TableError;
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
}

impl Failure {
    /// The errors as one line each.
    fn messages(&self) -> Vec<String> {
        match self {
            Failure::Input(message) => vec![message.clone()],
            Failure::Layout(errors) => errors.iter().map(ToString::to_string).collect(),
            Failure::Table(error) => vec![error.to_string()],
        }
    }

    /// The errors as JSON objects, each with its `message`.
    fn objects(&self) -> Vec<Value> {
        match self {
            Failure::Input(message) => vec![json!({"kind": "input", "message": message})],
            Failure::Layout(errors) => errors
                .iter()
                .map(|error| with_message(error, error))
                .collect(),
            Failure::Table(error) => vec![with_message(error, error)],
        }
    }

    /// The text for standard error.
    pub fn human(&self) -> String {
        let messages = self.messages();
        let mut text = String::new();
        for message in &messages {
            let _ = writeln!(text, "error: {message}");
        }
        if !matches!(self, Failure::Input(_)) {
            let _ = writeln!(text, "error: the layout has {} problem(s)", messages.len());
        }
        text
    }

    /// The JSON document for standard output.
    pub fn json(&self) -> String {
        render(&json!({"ok": false, "errors": self.objects()}))
    }
}

/// Serialize an error and add its `message`.
fn with_message(error: &impl Serialize, display: &impl std::fmt::Display) -> Value {
    let mut value = serde_json::to_value(error).unwrap_or_else(|_| json!({"kind": "unknown"}));
    if let Value::Object(map) = &mut value {
        map.insert("message".into(), Value::String(display.to_string()));
    }
    value
}

fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// The JSON document of a successful `check`.
pub fn report_json(report: &Report) -> String {
    render(&json!({"ok": true, "report": report}))
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
