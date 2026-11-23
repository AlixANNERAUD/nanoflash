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

// Flashing, detecting, merging images and checking a layout against a manifest.

use nanoflash::testing::{Event, HostToken, Transcript};

fn boards(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanoflash/tests/boards")
        .join(name)
}

fn transcript_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanoflash/tests/transcripts/espressif")
        .join(name)
}

fn images(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanoflash/tests/images")
        .join(name)
}

const USB_BOARD: &str = "esp32-s3-usb.toml";

fn flash_replaying(transcript: &str, extra: &[&str]) -> Output {
    let board = boards(USB_BOARD);
    let transcript = transcript_path(transcript);
    let mut arguments = vec![
        "flash",
        "--board",
        path_text(&board),
        "--replay",
        path_text(&transcript),
    ];
    arguments.extend_from_slice(extra);
    run(&arguments)
}

fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", text(&output.stdout)))
}

#[test]
fn flash_replays_a_transcript() {
    let output = flash_replaying("flash-esp32s3-usb.transcript", &["--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["report"]["verified"], true);
    assert_eq!(document["report"]["images"].as_array().unwrap().len(), 3);
}

#[test]
fn flash_prints_the_result_for_humans() {
    let output = flash_replaying("flash-esp32s3-usb.transcript", &[]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(
        stdout.starts_with("flashed esp32-s3-devkitc (esp32-s3, 8 MiB)\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  0x20000  application  8 KiB\n"),
        "{stdout}"
    );
    assert!(stdout.ends_with("verified\n"), "{stdout}");
    let stderr = text(&output.stderr);
    assert!(stderr.contains("connecting\n"), "{stderr}");
    assert!(stderr.contains("writing"), "{stderr}");
    assert!(stderr.contains("verifying\n"), "{stderr}");
    assert!(stderr.contains("resetting\n"), "{stderr}");
}

#[test]
fn flash_progress_as_json_lines() {
    let output = flash_replaying("flash-esp32s3-usb.transcript", &["--progress", "json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(!stderr.is_empty());
    for line in stderr.lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        assert!(event["stage"].is_string(), "{line}");
    }
}

#[test]
fn verification_failure_exits_3() {
    let output = flash_replaying("flash-md5-mismatch.transcript", &["--json"]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["ok"], false);
    assert_eq!(document["errors"][0]["kind"], "verification-failed");
    assert_eq!(document["errors"][0]["class"], "verification");
}

#[test]
fn device_errors_exit_2() {
    let output = flash_replaying("flash-no-sync.transcript", &[]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("BOOT"));
}

#[test]
fn disconnect_exits_2() {
    let output = flash_replaying("flash-disconnect.transcript", &["--json"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["errors"][0]["class"], "device");
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("disconnected")
    );
}

fn manifest_with_image(directory: &Path, path: &str) -> PathBuf {
    let manifest = directory.join("manifest.toml");
    let layout = fixture(ESP32);
    fs::write(
        &manifest,
        format!(
            "[[board]]\nidentifier = \"b\"\nprotocol = \"espressif\"\nchip = \"esp32-s3\"\n\
             layout = \"{}\"\nreset = \"espressif-usb-serial-jtag\"\n\
             application-devices = [{{ vendor = 0x303a, product = 0x1001 }}]\n\n\
             [[board.image]]\npartition = \"application\"\npath = \"{path}\"\n",
            path_text(&layout)
        ),
    )
    .unwrap();
    manifest
}

#[test]
fn input_errors_exit_1_before_the_device() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = manifest_with_image(directory.path(), "missing.bin");
    let transcript = transcript_path("identity-only.transcript");
    let output = run(&[
        "flash",
        "--board",
        path_text(&manifest),
        "--replay",
        path_text(&transcript),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["errors"][0]["class"], "input");
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("missing.bin")
    );
}

#[test]
fn url_paths_are_refused_natively() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = manifest_with_image(directory.path(), "https://example.com/a.bin");
    let transcript = transcript_path("identity-only.transcript");
    let output = run(&[
        "flash",
        "--board",
        path_text(&manifest),
        "--replay",
        path_text(&transcript),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let message = document(&output)["errors"][0]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("web package"), "{message}");
}

/// Whether the bytes the host wrote match the tokens of the transcript line, a `..` standing for
/// one or more bytes up to the next literal one.
fn host_matches(tokens: &[HostToken], bytes: &[u8]) -> bool {
    let mut position = 0;
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        match token {
            HostToken::Byte(byte) => {
                if bytes.get(position) != Some(byte) {
                    return false;
                }
                position += 1;
            }
            HostToken::Any => {
                let Some(HostToken::Byte(next)) = tokens.get(index + 1) else {
                    return false;
                };
                let Some(length) = bytes
                    .get(position + 1..)
                    .and_then(|rest| rest.iter().position(|byte| byte == next))
                else {
                    return false;
                };
                position += 1 + length;
            }
        }
        index += 1;
    }
    position == bytes.len()
}

#[test]
fn record_writes_the_transcript() {
    let directory = tempfile::tempdir().unwrap();
    let recorded = directory.path().join("recorded.transcript");
    let output = flash_replaying(
        "flash-esp32s3-usb.transcript",
        &["--record", path_text(&recorded), "--json"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let original = Transcript::parse(
        &fs::read_to_string(transcript_path("flash-esp32s3-usb.transcript")).unwrap(),
    )
    .unwrap();
    let copy = Transcript::parse(&fs::read_to_string(&recorded).unwrap()).unwrap();
    assert_eq!(copy.identity, original.identity);
    assert_eq!(copy.events.len(), original.events.len());
    for (expected, actual) in original.events.iter().zip(&copy.events) {
        match (&expected.event, &actual.event) {
            (Event::Host(tokens), Event::Host(written)) => {
                let bytes: Vec<u8> = written
                    .iter()
                    .map(|token| match token {
                        HostToken::Byte(byte) => *byte,
                        HostToken::Any => panic!("a recorded line has no wildcard"),
                    })
                    .collect();
                assert!(host_matches(tokens, &bytes), "line {}", expected.line);
            }
            (event, other) => assert_eq!(event, other, "line {}", expected.line),
        }
    }
}

#[test]
fn record_writes_the_transcript_when_the_command_fails() {
    let directory = tempfile::tempdir().unwrap();
    let recorded = directory.path().join("failed.transcript");
    let output = flash_replaying(
        "flash-md5-mismatch.transcript",
        &["--record", path_text(&recorded)],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(Transcript::parse(&fs::read_to_string(&recorded).unwrap()).is_ok());
}

fn detect_replaying(extra: &[&str]) -> Output {
    let transcript = transcript_path("detect-esp32s3-usb.transcript");
    let mut arguments = vec!["detect", "--replay", path_text(&transcript)];
    arguments.extend_from_slice(extra);
    run(&arguments)
}

#[test]
fn detect_prints_the_chip() {
    let output = detect_replaying(&[]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert_eq!(
        stdout,
        "protocol espressif\nchip esp32-s3\nflash 8 MiB\nusb 303a:1001\nsecure download mode no\n"
    );
    let output = detect_replaying(&["--json"]);
    let document = document(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["chip"]["chip"], "esp32-s3");
}

#[test]
fn detect_needs_a_port() {
    let output = run(&["detect", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let message = document(&output)["errors"][0]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("--port"), "{message}");
}

#[test]
fn detect_without_identity_asks_for_a_protocol() {
    let directory = tempfile::tempdir().unwrap();
    let transcript = directory.path().join("bridge.transcript");
    fs::write(&transcript, "! timeout\n").unwrap();
    let output = run(&["detect", "--replay", path_text(&transcript), "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let document = document(&output);
    assert_eq!(document["errors"][0]["kind"], "protocol-required");
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("--protocol")
    );
    let output = run(&[
        "detect",
        "--replay",
        path_text(&transcript),
        "--protocol",
        "espressif",
    ]);
    assert_ne!(output.status.code(), Some(1), "{}", text(&output.stderr));
}

#[test]
fn image_build_writes_the_merged_image() {
    let directory = tempfile::tempdir().unwrap();
    let board = boards(USB_BOARD);
    let merged = directory.path().join("out.bin");
    let output = run(&[
        "image",
        "build",
        "--board",
        path_text(&board),
        "--output",
        path_text(&merged),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let bytes = fs::read(&merged).unwrap();
    assert_eq!(
        text(&output.stdout),
        format!("wrote {} bytes to {}\n", bytes.len(), merged.display())
    );
    assert_eq!(bytes.len(), 0x20000 + 8192);
    assert_eq!(
        bytes[..4096],
        fs::read(images("bootloader.bin")).unwrap()[..]
    );
    assert_eq!(
        bytes[0x20000..],
        fs::read(images("application.bin")).unwrap()[..]
    );
    assert_eq!(bytes[0x8000..0x8004], [0xAA, 0x50, 0x01, 0x02]);

    let filled = directory.path().join("filled.bin");
    let output = run(&[
        "image",
        "build",
        "--board",
        path_text(&board),
        "--output",
        path_text(&filled),
        "--fill",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["bytes"], 8 * 1024 * 1024);
    assert_eq!(fs::metadata(&filled).unwrap().len(), 8 * 1024 * 1024);
}

#[test]
fn layout_check_with_a_manifest() {
    let layout = fixture(ESP32);
    let board = boards(USB_BOARD);
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--manifest",
        path_text(&board),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    let row = |name: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} row in {stdout}"))
            .to_string()
    };
    assert!(row("bootloader").contains("4 KiB"), "{stdout}");
    assert!(row("application").contains("8 KiB"), "{stdout}");
}

#[test]
fn layout_check_warns_on_a_different_table() {
    let directory = tempfile::tempdir().unwrap();
    let table = directory.path().join("table.bin");
    fs::write(&table, vec![0u8; 0xC00]).unwrap();
    let manifest = directory.path().join("manifest.toml");
    let layout = fixture(ESP32);
    fs::write(
        &manifest,
        format!(
            "[[board]]\nidentifier = \"b\"\nprotocol = \"espressif\"\nchip = \"esp32-s3\"\n\
             layout = \"{}\"\n\n[[board.image]]\npartition = \"partition-table\"\npath = \"{}\"\n",
            path_text(&layout),
            path_text(&table)
        ),
    )
    .unwrap();
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--manifest",
        path_text(&manifest),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("warning:"));
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--manifest",
        path_text(&manifest),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        document(&output)["warnings"][0]["kind"],
        "partition-table-differs"
    );
}

#[test]
fn image_and_manifest_conflict() {
    let layout = fixture(ESP32);
    let board = boards(USB_BOARD);
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--image",
        "a=b",
        "--manifest",
        path_text(&board),
    ]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn layout_check_names_the_boards_when_none_uses_the_layout() {
    let layout = fixture("sd-card.toml");
    let board = boards(USB_BOARD);
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--manifest",
        path_text(&board),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("esp32-s3-devkitc"));
}

#[test]
fn a_replay_that_diverges_exits_2() {
    let directory = tempfile::tempdir().unwrap();
    let transcript = directory.path().join("short.transcript");
    fs::write(&transcript, "! identity 303a:1001\n> 01\n").unwrap();
    let board = boards(USB_BOARD);
    let output = run(&[
        "flash",
        "--board",
        path_text(&board),
        "--replay",
        path_text(&transcript),
    ]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
}
