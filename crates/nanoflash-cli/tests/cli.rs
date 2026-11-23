//! End-to-end tests of the `nanoflash` command.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanoflash/tests/layouts")
        .join(name)
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nanoflash"))
        .args(arguments)
        .output()
        .expect("the nanoflash binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

const ESP32: &str = "esp32-s3-8mb.toml";

#[test]
fn check_prints_the_table() {
    let layout = fixture(ESP32);
    let output = run(&["layout", "check", path_text(&layout)]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = text(&output.stdout);
    assert!(
        stdout.starts_with("flash 8 MiB (0x800000), unallocated "),
        "{stdout}"
    );
    assert!(stdout.contains("application"), "{stdout}");
    assert!(stdout.contains("0x20000"), "{stdout}");
    assert!(stdout.contains("partition-table"), "{stdout}");
    assert!(stdout.contains("headroom"), "{stdout}");
}

#[test]
fn check_json_is_one_document() {
    let layout = fixture(ESP32);
    let output = run(&["layout", "check", path_text(&layout), "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(
        document["report"]["partitions"].as_array().unwrap().len(),
        8
    );
}

fn write_overlapping_layout(directory: &Path) -> PathBuf {
    let path = directory.join("overlap.toml");
    fs::write(
        &path,
        "[flash]\nsize = \"1 MiB\"\nalignment = \"4 KiB\"\n\n\
         [[partition]]\nname = \"a\"\nrole = \"firmware\"\noffset = 0\nsize = \"64 KiB\"\n\n\
         [[partition]]\nname = \"b\"\nrole = \"storage\"\noffset = 0x8000\nsize = \"64 KiB\"\n",
    )
    .unwrap();
    path
}

#[test]
fn check_fails_on_overlap_with_exit_code_1() {
    let directory = tempfile::tempdir().unwrap();
    let layout = write_overlapping_layout(directory.path());
    let output = run(&["layout", "check", path_text(&layout)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("error: "), "{stderr}");
    assert!(stderr.contains("overlap"), "{stderr}");
    assert!(
        stderr
            .trim_end()
            .ends_with("error: the layout has 1 problem(s)"),
        "{stderr}"
    );

    let output = run(&["layout", "check", path_text(&layout), "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], false);
    assert_eq!(document["errors"][0]["kind"], "overlap");
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("overlap")
    );
}

fn image_of(directory: &Path, size: usize) -> PathBuf {
    let path = directory.join("image.bin");
    fs::write(&path, vec![0u8; size]).unwrap();
    path
}

#[test]
fn check_image_headroom() {
    let directory = tempfile::tempdir().unwrap();
    let image = image_of(directory.path(), 0xC0000);
    let layout = fixture(ESP32);
    let argument = format!("application={}", path_text(&image));
    let output = run(&["layout", "check", path_text(&layout), "--image", &argument]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        text(&output.stdout).contains("256 KiB (25%)"),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn image_too_large_fails() {
    let directory = tempfile::tempdir().unwrap();
    let image = image_of(directory.path(), 0x100001);
    let layout = fixture(ESP32);
    let argument = format!("application={}", path_text(&image));
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--image",
        &argument,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["errors"][0]["kind"], "image-too-large");
}

#[test]
fn bad_image_argument() {
    let layout = fixture(ESP32);
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--image",
        "application",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("PARTITION=FILE"),
        "{}",
        text(&output.stderr)
    );

    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--image",
        "application",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["errors"][0]["kind"], "input");
}

#[test]
fn missing_layout_file_is_an_input_error() {
    let output = run(&["layout", "check", "/nonexistent/layout.toml"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("error: "));
}

#[test]
fn write_espressif_is_0xc00_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("table.bin");
    let layout = fixture(ESP32);
    let output = run(&[
        "layout",
        "write",
        path_text(&layout),
        "--format",
        "espressif",
        "--output",
        path_text(&output_path),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(fs::read(&output_path).unwrap().len(), 0xC00);
    assert!(
        text(&output.stdout).contains("(3072 bytes)"),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn write_mbr_is_512_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("mbr.bin");
    let layout = fixture("sd-card.toml");
    let output = run(&[
        "layout",
        "write",
        path_text(&layout),
        "--format",
        "mbr",
        "--output",
        path_text(&output_path),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(fs::read(&output_path).unwrap().len(), 512);
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(document["bytes"], 512);
}

#[test]
fn write_constants_matches_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("constants.rs");
    let layout = fixture(ESP32);
    let output = run(&[
        "layout",
        "write",
        path_text(&layout),
        "--format",
        "constants",
        "--output",
        path_text(&output_path),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let resolved = nanoflash::layout::Layout::from_toml(&fs::read_to_string(&layout).unwrap())
        .unwrap()
        .resolve()
        .unwrap();
    let expected = nanoflash::table::linker::rust_constants(&resolved).unwrap();
    assert_eq!(fs::read_to_string(&output_path).unwrap(), expected);
}

#[test]
fn write_linker_regions() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("memory.x");
    let layout = fixture("nrf52840-arduino.toml");
    let output = run(&[
        "layout",
        "write",
        path_text(&layout),
        "--format",
        "linker",
        "--output",
        path_text(&output_path),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(fs::read_to_string(&output_path).unwrap().contains("MEMORY"));
    assert!(
        text(&output.stdout).starts_with("wrote linker output to "),
        "{}",
        text(&output.stdout)
    );
}

#[test]
fn bad_format_exits_1_with_json_when_asked() {
    let layout = fixture(ESP32);
    let arguments = [
        "layout",
        "write",
        path_text(&layout),
        "--format",
        "bogus",
        "--output",
        "x",
    ];
    let output = run(&arguments);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        text(&output.stderr).contains("bogus"),
        "{}",
        text(&output.stderr)
    );

    let mut with_json = arguments.to_vec();
    with_json.push("--json");
    let output = run(&with_json);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], false);
    assert_eq!(document["errors"].as_array().unwrap().len(), 1);
    assert_eq!(document["errors"][0]["kind"], "input");
    let message = document["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("bogus") && !message.contains('\u{1b}'),
        "{message}"
    );
}

#[test]
fn help_and_version_exit_0() {
    for argument in ["--help", "--version"] {
        let output = run(&[argument]);
        assert_eq!(output.status.code(), Some(0), "{argument}");
        assert!(!output.stdout.is_empty());
    }
}

#[test]
fn image_directory_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let layout = fixture(ESP32);
    let argument = format!("application={}", path_text(directory.path()));
    let output = run(&["layout", "check", path_text(&layout), "--image", &argument]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("not a regular file"), "{stderr}");
    assert!(stderr.contains(path_text(directory.path())), "{stderr}");
}

#[test]
fn closed_stdout_does_not_panic() {
    let layout = fixture(ESP32);
    let mut child = Command::new(env!("CARGO_BIN_EXE_nanoflash"))
        .args(["layout", "check", path_text(&layout)])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn write_table_error_exits_1() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("mbr.bin");
    let layout = fixture(ESP32);
    let output = run(&[
        "layout",
        "write",
        path_text(&layout),
        "--format",
        "mbr",
        "--output",
        path_text(&output_path),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], false);
    assert!(document["errors"][0]["writer"].is_string());
    assert!(!output_path.exists());
}
