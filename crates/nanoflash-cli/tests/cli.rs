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
fn bad_format_lists_the_possible_values_and_the_help_hint() {
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
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("[possible values: espressif, mbr, linker, constants]"),
        "{stderr}"
    );
    assert!(stderr.contains("\nUsage: nanoflash"), "{stderr}");
    assert!(
        stderr.ends_with("For more information, try '--help'.\n"),
        "{stderr}"
    );

    let mut with_json = arguments.to_vec();
    with_json.push("--json");
    let output = run(&with_json);
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = document["errors"][0]["message"].as_str().unwrap();
    assert!(message.contains("possible values"), "{message}");
    assert!(
        !message.contains("Usage:") && !message.contains("--help"),
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
    assert_eq!(
        document["errors"][0]["during"],
        "writing \"bootloader\" at 0x0"
    );
    let message = document["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("the device disconnected while writing \"bootloader\" at 0x0"),
        "{message}"
    );
    let output = flash_replaying("flash-disconnect.transcript", &["--progress", "quiet"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: the device disconnected while writing \"bootloader\" at 0x0: check the cable"
        ),
        "{stderr}"
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
            HostToken::Count(count) => {
                if position + count > bytes.len() {
                    return false;
                }
                position += count;
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
                        HostToken::Any | HostToken::Count(_) => {
                            panic!("a recorded line has no wildcard")
                        }
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
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
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

#[test]
fn port_and_replay_conflict() {
    let transcript = transcript_path("identity-only.transcript");
    let output = run(&[
        "detect",
        "--port",
        "/dev/null",
        "--replay",
        path_text(&transcript),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("cannot be used with"));
}

#[test]
fn image_build_refuses_offsets_beyond_four_gibibytes() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("small.bin"), [0u8; 16]).unwrap();
    for offset in ["1099511627776", "18446744073709486080"] {
        let manifest = directory.path().join("manifest.json");
        fs::write(
            &manifest,
            format!(
                r#"{{"builds":[{{"chipFamily":"ESP32","parts":[{{"path":"small.bin","offset":{offset}}}]}}]}}"#
            ),
        )
        .unwrap();
        let merged = directory.path().join("out.bin");
        let output = run(&[
            "image",
            "build",
            "--board",
            path_text(&manifest),
            "--output",
            path_text(&merged),
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
        let document = document(&output);
        assert_eq!(document["errors"][0]["kind"], "outside-address-range");
        assert!(!merged.exists());
    }
}

#[test]
fn secure_download_mode_is_flashed_without_verification() {
    // The connection of connect-secure-download, then the writes of flash-esp32s3-usb without
    // its MD5 checks.
    let flash = fs::read_to_string(transcript_path("flash-esp32s3-usb.transcript")).unwrap();
    let writes: String = format!("> c0 00 10 {}", flash.split_once("> c0 00 10 ").unwrap().1)
        .lines()
        .filter(|line| !line.starts_with("> c0 00 13") && !line.starts_with("< c0 01 13"))
        .map(|line| format!("{line}\n"))
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let transcript = directory.path().join("secure.transcript");
    fs::write(
        &transcript,
        fs::read_to_string(transcript_path("connect-secure-download.transcript")).unwrap()
            + &writes,
    )
    .unwrap();
    let board = boards(USB_BOARD);
    let output = run(&[
        "flash",
        "--board",
        path_text(&board),
        "--replay",
        path_text(&transcript),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(
        stdout.ends_with("not verified (secure download mode)\n"),
        "{stdout}"
    );
    assert!(
        text(&output.stderr).contains("warning: the chip is in secure download mode"),
        "{}",
        text(&output.stderr)
    );
}

// SAM-BA: the Arduino Nano 33 BLE.

fn samba_transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanoflash/tests/transcripts/samba")
        .join(name)
}

fn hardware(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../hardware/nano33ble")
        .join(name)
}

fn flash_nano_33_ble(manifest: &Path, transcript: &str) -> Output {
    let transcript = samba_transcript(transcript);
    run(&[
        "flash",
        "--board",
        path_text(manifest),
        "--replay",
        path_text(&transcript),
        "--json",
    ])
}

#[test]
fn flashes_a_nano_33_ble_from_a_transcript() {
    let board = boards("nano33ble.toml");
    let output = flash_nano_33_ble(&board, "flash-nano33ble.transcript");
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["report"]["chip"]["chip"], "nrf52840");
    assert_eq!(document["report"]["chip"]["writable_from"], 65536);
    assert_eq!(document["report"]["verified"], true);
}

#[test]
fn the_samd21_erase_to_the_end_of_flash_is_a_warning() {
    let board = boards("mkr-wifi-1010.toml");
    let transcript = samba_transcript("flash-mkrwifi1010.transcript");
    let arguments = [
        "flash",
        "--board",
        path_text(&board),
        "--replay",
        path_text(&transcript),
    ];
    let message = "the bootloader erased the flash from 0x2000 to its end at 0x40000 before writing, so what was stored there besides the images is gone";
    let human = run(&arguments);
    assert_eq!(human.status.code(), Some(0), "{}", text(&human.stderr));
    assert!(
        text(&human.stderr).contains(&format!("warning: {message}\n")),
        "{}",
        text(&human.stderr)
    );
    let json = run(&[&arguments[..], &["--json"]].concat());
    assert_eq!(json.status.code(), Some(0), "{}", text(&json.stderr));
    let warning = &document(&json)["report"]["warnings"][0];
    assert_eq!(warning["kind"], "erases-to-end");
    assert_eq!(warning["from"], 0x2000);
    assert_eq!(warning["to"], 0x4_0000);
    assert_eq!(warning["partitions"], serde_json::json!([]));
    assert_eq!(warning["message"], message);
}

#[test]
fn samba_crc_mismatch_exits_3() {
    let board = boards("nano33ble.toml");
    let output = flash_nano_33_ble(&board, "flash-nano33ble-crc-mismatch.transcript");
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["errors"][0]["kind"], "verification-failed");
    assert_eq!(document["errors"][0]["checksum"], "CRC-16");
}

#[test]
fn samba_image_below_the_application_exits_1() {
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let manifest = directory.path().join("manifest.toml");
    let layout = hardware("layout.toml");
    let application = images("application.bin");
    let bootloader = images("bootloader.bin");
    fs::write(
        &manifest,
        format!(
            "[[board]]\nidentifier = \"arduino-nano-33-ble\"\nprotocol = \"samba\"\n\
             chip = \"nrf52840\"\nlayout = \"{}\"\nreset = \"touch-1200\"\n\
             application-devices = [{{ vendor = 0x2341, product = 0x805a }}]\n\
             bootloader-devices = [{{ vendor = 0x2341, product = 0x005a }}]\n\n\
             [[board.image]]\npartition = \"bootloader\"\npath = \"{}\"\n\n\
             [[board.image]]\npartition = \"application\"\npath = \"{}\"\n",
            path_text(&layout),
            path_text(&bootloader),
            path_text(&application)
        ),
    )
    .unwrap();
    let output = flash_nano_33_ble(&manifest, "flash-below-application.transcript");
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["errors"][0]["class"], "input");
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("0x10000"),
        "{document}"
    );
}

#[test]
fn detects_a_nano_33_ble() {
    let transcript = samba_transcript("detect-nano33ble.transcript");
    for extra in [&[][..], &["--protocol", "samba"][..]] {
        let mut arguments = vec!["detect", "--replay", path_text(&transcript)];
        arguments.extend_from_slice(extra);
        let output = run(&arguments);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        let stdout = text(&output.stdout);
        for line in [
            "protocol samba",
            "chip nrf52840",
            "flash 1 MiB",
            "writable from 0x10000",
        ] {
            assert!(
                stdout.lines().any(|found| found == line),
                "{line} in {stdout}"
            );
        }
    }
}

#[test]
fn the_nano_33_ble_layout_matches_nanoglia() {
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let layout = hardware("layout.toml");
    let write = |format: &str, name: &str| {
        let path = directory.path().join(name);
        let output = run(&[
            "layout",
            "write",
            path_text(&layout),
            "--format",
            format,
            "--output",
            path_text(&path),
        ]);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        fs::read_to_string(path).unwrap()
    };
    // nanoglia's hardware/nano33ble/memory.x: FLASH ORIGIN 0x00010000, LENGTH 1024K - 64K.
    assert_eq!(
        write("linker", "memory.x"),
        "/* Generated by nanoflash from the board layout. Do not edit. */\nMEMORY\n{\n  \
         BOOTLOADER : ORIGIN = 0x00000000, LENGTH = 0x00010000\n  \
         FLASH : ORIGIN = 0x00010000, LENGTH = 0x000F0000\n}\n"
    );
    let constants = write("constants", "constants.rs");
    assert!(constants.contains("0x0001_0000"), "{constants}");
    assert!(constants.contains("0x000F_0000"), "{constants}");
}

#[test]
fn the_hardware_manifest_parses() {
    let manifest = hardware("manifest.toml");
    let parsed =
        nanoflash::manifest::Manifest::parse(&fs::read_to_string(&manifest).unwrap()).unwrap();
    assert_eq!(parsed.boards.len(), 1);
    assert_eq!(
        parsed.boards[0].protocol,
        nanoflash::manifest::Protocol::Samba
    );
    assert_eq!(parsed.boards[0].chip, "nrf52840");
    let layout = hardware("layout.toml");
    let output = run(&["layout", "check", path_text(&layout)]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
}

// arduino/BOSSA's emulated SAM-BA bootloader, recorded by hardware/samba-emulator/run.sh.

#[test]
fn replays_the_emulator_recording() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let board = repository.join("hardware/samba-emulator/nano33ble.toml");
    let transcript = samba_transcript("recorded/emulator-nano33ble.transcript");
    let output = run(&[
        "flash",
        "--board",
        path_text(&board),
        "--replay",
        path_text(&transcript),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["report"]["verified"], true);
}

#[test]
fn layout_check_counts_hex_images_decoded() {
    let layout = fixture("uno.toml");
    let hex = images("avr-application.hex");
    let argument = format!("application={}", path_text(&hex));
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--image",
        &argument,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    let entry = document["report"]["partitions"]
        .as_array()
        .or_else(|| document["partitions"].as_array())
        .and_then(|partitions| {
            partitions
                .iter()
                .find(|entry| entry["name"] == "application")
        })
        .unwrap_or_else(|| panic!("{document}"));
    assert_eq!(entry["image_size"], 300, "{entry}");
}

#[test]
fn layout_check_names_the_line_of_a_bad_hex_image() {
    let directory = tempfile::tempdir().unwrap();
    let hex = directory.path().join("broken.hex");
    fs::write(&hex, ":0100000055AB\n:00000001FF\n").unwrap();
    let layout = fixture("uno.toml");
    let argument = format!("application={}", path_text(&hex));
    let output = run(&["layout", "check", path_text(&layout), "--image", &argument]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("is invalid at line 1"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn layout_check_refuses_a_hex_image_that_starts_elsewhere() {
    // The fixture starts at 0x0; the bootloader partition of the Uno starts at 0x7e00.
    let layout = fixture("uno.toml");
    let hex = images("avr-application.hex");
    let argument = format!("bootloader={}", path_text(&hex));
    let output = run(&[
        "layout",
        "check",
        path_text(&layout),
        "--image",
        &argument,
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(
        document["errors"][0]["kind"], "misplaced-intel-hex",
        "{document}"
    );
    let message = document["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("starts at 0x0")
            && message.contains("the partition \"bootloader\" it is written to starts at 0x7e00"),
        "{message}"
    );
}

// AVR: Arduino Uno, Nano, Mega 2560 and Leonardo.

fn avr_transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanoflash/tests/transcripts/avr")
        .join(name)
}

fn flash_uno(transcript: &str) -> Output {
    let board = boards("uno.toml");
    let transcript = avr_transcript(transcript);
    run(&[
        "flash",
        "--board",
        path_text(&board),
        "--replay",
        path_text(&transcript),
        "--json",
    ])
}

#[test]
fn flashes_an_uno_from_a_transcript() {
    let output = flash_uno("flash-uno.transcript");
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["report"]["chip"]["writable_to"], 32256);
    assert_eq!(document["report"]["verified"], true);
}

#[test]
fn avr_read_back_difference_exits_3() {
    let output = flash_uno("flash-uno-read-back.transcript");
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    let document = document(&output);
    assert_eq!(document["errors"][0]["kind"], "read-back-differs");
    assert_eq!(document["errors"][0]["offset"], 133);
}

#[test]
fn avr_image_over_the_bootloader_exits_1() {
    let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let image = directory.path().join("sketch.bin");
    fs::write(&image, vec![0x55; 0x7E80]).unwrap();
    let manifest = directory.path().join("manifest.toml");
    fs::write(
        &manifest,
        format!(
            "[[board]]\nidentifier = \"arduino-uno-r3\"\nprotocol = \"stk500v1\"\n\
             chip = \"atmega328p\"\n\n[[board.image]]\noffset = 0\npath = \"{}\"\n",
            path_text(&image)
        ),
    )
    .unwrap();
    // The Uno's connection, then the end of the session: nothing is written.
    let connect = fs::read_to_string(avr_transcript("flash-uno.transcript")).unwrap();
    let signature = connect.find("< 14 1e 95 0f 10").unwrap() + "< 14 1e 95 0f 10\n".len();
    let transcript = directory.path().join("over-the-bootloader.transcript");
    fs::write(
        &transcript,
        format!("{}> 51 20\n< 14 10\n", &connect[..signature]),
    )
    .unwrap();
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
            .contains("0x7e00"),
        "{document}"
    );
}

#[test]
fn detects_an_uno_and_a_leonardo() {
    let uno = avr_transcript("detect-uno.transcript");
    let output = run(&["detect", "--replay", path_text(&uno)]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    for line in ["protocol stk500v1", "chip atmega328p", "writable to 0x7e00"] {
        assert!(
            stdout.lines().any(|found| found == line),
            "{line} in {stdout}"
        );
    }
    // The protocols also go by their names in Arduino's boards.txt, as in manifests: `arduino`
    // for the Uno, `wiring` for the Mega 2560 (its connection, then CMD_LEAVE_PROGMODE_ISP).
    let directory = tempfile::tempdir().unwrap();
    let mega = directory.path().join("detect-mega2560.transcript");
    let connect = fs::read_to_string(avr_transcript("connect-mega2560.transcript")).unwrap();
    fs::write(
        &mega,
        connect + "> 1b 05 00 03 0e 11 01 01 02\n< 1b 05 00 02 0e 11 00 03\n",
    )
    .unwrap();
    for (alias, transcript, protocol) in
        [("arduino", &uno, "stk500v1"), ("wiring", &mega, "stk500v2")]
    {
        let output = run(&[
            "detect",
            "--replay",
            path_text(transcript),
            "--protocol",
            alias,
        ]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{alias}: {}",
            text(&output.stderr)
        );
        let stdout = text(&output.stdout);
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("protocol {protocol}")),
            "{alias}: {stdout}"
        );
    }
    let leonardo = avr_transcript("detect-leonardo.transcript");
    let output = run(&[
        "detect",
        "--replay",
        path_text(&leonardo),
        "--protocol",
        "avr109",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("protocol avr109"), "{stdout}");
    assert!(stdout.contains("chip atmega32u4"), "{stdout}");
}

#[test]
fn the_avr_board_files_check() {
    use nanoflash::manifest::{Manifest, Protocol};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hardware");
    let boards = [
        ("uno", Protocol::Stk500v1, "atmega328p", 0x7E00),
        ("nano", Protocol::Stk500v1, "atmega328p", 0x7800),
        ("mega2560", Protocol::Stk500v2, "atmega2560", 0x3_C000),
        ("leonardo", Protocol::Avr109, "atmega32u4", 0x7000),
    ];
    for (name, protocol, chip, application) in boards {
        let layout = root.join(name).join("layout.toml");
        let output = run(&["layout", "check", path_text(&layout)]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{name}: {}",
            text(&output.stderr)
        );
        let manifest = fs::read_to_string(root.join(name).join("manifest.toml")).unwrap();
        let parsed = Manifest::parse(&manifest).unwrap();
        assert_eq!(parsed.boards.len(), 1, "{name}");
        assert_eq!(parsed.boards[0].protocol, protocol, "{name}");
        assert_eq!(parsed.boards[0].chip, chip, "{name}");
        // The JSON twin, for the browser, lists the same board.
        let json = fs::read_to_string(root.join(name).join("manifest.json")).unwrap();
        assert_eq!(Manifest::parse(&json).unwrap(), parsed, "{name}");
        let layout = nanoflash::layout::Layout::from_toml(&fs::read_to_string(&layout).unwrap())
            .unwrap()
            .resolve()
            .unwrap();
        let size = layout
            .partitions
            .iter()
            .find(|partition| partition.definition.name == "application")
            .map(|partition| partition.size);
        assert_eq!(size, Some(application), "{name}");
    }
}

#[test]
fn the_library_layout_fixtures_are_the_board_files() {
    // The library's tests cannot read hardware/, which is not part of its package: they use these
    // copies.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hardware");
    for name in ["uno", "nano"] {
        let board = fs::read_to_string(root.join(name).join("layout.toml")).unwrap();
        let copy = fs::read_to_string(fixture(&format!("{name}.toml"))).unwrap();
        assert_eq!(copy, board, "{name}");
    }
}

// The Arduino STK500 bootloaders emulated by simavr, recorded by hardware/simavr/run.sh.

#[test]
fn replays_the_simavr_recordings() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let recordings = [
        ("uno.toml", "simavr-optiboot.transcript", None),
        ("nano.toml", "simavr-atmegaboot.transcript", Some("57600")),
        ("mega2560.toml", "simavr-mega2560.transcript", None),
    ];
    for (board, transcript, baud_rate) in recordings {
        let board = repository.join("hardware/simavr").join(board);
        let transcript = avr_transcript("recorded").join(transcript);
        let mut arguments = vec![
            "flash",
            "--board",
            path_text(&board),
            "--replay",
            path_text(&transcript),
            "--json",
        ];
        if let Some(rate) = baud_rate {
            arguments.extend_from_slice(&["--baud-rate", rate]);
        }
        let output = run(&arguments);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}: {}",
            transcript.display(),
            text(&output.stderr)
        );
        let document = document(&output);
        assert_eq!(document["ok"], true, "{}", transcript.display());
        assert_eq!(
            document["report"]["verified"],
            true,
            "{}",
            transcript.display()
        );
    }
}
