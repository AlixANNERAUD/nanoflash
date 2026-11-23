//! Reads the generated tables back with the tools the ecosystem already trusts.
//!
//! A missing tool skips the test, unless `NANOFLASH_EXTERNAL_PARSERS` is set to `1`, in which
//! case it fails. `NANOFLASH_GEN_ESP32PART` is the path to ESP-IDF's `gen_esp32part.py`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use nanoflash::layout::{Layout, ResolvedLayout};
use nanoflash::table::{espressif, mbr};

fn strict() -> bool {
    std::env::var("NANOFLASH_EXTERNAL_PARSERS").is_ok_and(|value| value == "1")
}

fn fixture(text: &str) -> ResolvedLayout {
    Layout::from_toml(text).unwrap().resolve().unwrap()
}

/// A unique directory outside the repository, removed on drop.
struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(name: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("nanoflash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn find_on_path(program: &str, extra: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(extra.iter().map(PathBuf::from))
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

/// Returns whether the test must stop here because the tool is missing.
fn missing(tool: &str) -> bool {
    assert!(
        !strict(),
        "{tool} is required because NANOFLASH_EXTERNAL_PARSERS=1"
    );
    eprintln!("skipping: {tool} not found");
    true
}

fn output_lines(program: &Path, arguments: &[&Path], directory: &Path) -> Vec<String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} failed: {}",
        program.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn gen_esp32part_reads_our_table() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = match std::env::var_os("NANOFLASH_GEN_ESP32PART") {
        Some(path) => workspace.join(path),
        None => workspace.join("target/external/gen_esp32part.py"),
    };
    let python = find_on_path("python3", &[]);
    let (Some(python), true) = (python, script.is_file()) else {
        if missing("python3 and gen_esp32part.py") {
            return;
        }
        unreachable!();
    };

    let table = espressif::write(&fixture(include_str!("layouts/esp32-s3-8mb.toml"))).unwrap();
    let directory = TemporaryDirectory::new("gen-esp32part");
    let binary = directory.0.join("table.bin");
    fs::write(&binary, table).unwrap();

    let script = script.canonicalize().unwrap();
    let lines = output_lines(&python, &[Path::new("-I"), &script, &binary], &directory.0);
    let partitions: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| !line.starts_with('#') && line.contains(','))
        .collect();
    assert_eq!(
        partitions,
        [
            "nvs,data,nvs,0x9000,24K,",
            "otadata,data,ota,0xf000,8K,",
            "application,app,factory,0x20000,1M,",
            "ota-0,app,ota_0,0x120000,1M,",
            "ota-1,app,ota_1,0x220000,1M,",
            "storage,data,littlefs,0x320000,4992K,",
        ],
        "gen_esp32part.py printed:\n{}",
        lines.join("\n")
    );
}

#[test]
fn sfdisk_reads_our_mbr() {
    let Some(sfdisk) = find_on_path("sfdisk", &["/usr/sbin", "/sbin"]) else {
        if missing("sfdisk") {
            return;
        }
        unreachable!();
    };

    let layout = fixture(include_str!("layouts/sd-card.toml"));
    let sector = mbr::write(&layout).unwrap();
    let directory = TemporaryDirectory::new("sfdisk");
    let image = directory.0.join("card.img");
    let file = File::create(&image).unwrap();
    file.set_len(layout.flash.size).unwrap();
    drop(file);
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().write(true).open(&image).unwrap();
        file.write_all(&sector).unwrap();
    }

    let lines = output_lines(&sfdisk, &[Path::new("--dump"), &image], &directory.0);
    let entries: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with(image.to_str().unwrap()))
        .collect();
    let expected = [
        (0x10_0000_u64, 16 * 1024 * 1024_u64, "c"),
        (0x110_0000, 47 * 1024 * 1024, "83"),
    ];
    assert_eq!(
        entries.len(),
        expected.len(),
        "sfdisk printed:\n{}",
        lines.join("\n")
    );
    for (entry, (offset, size, kind)) in entries.iter().zip(expected) {
        let (_, fields) = entry.split_once(" : ").unwrap();
        let fields: Vec<(&str, &str)> = fields
            .split(',')
            .map(|field| {
                let (name, value) = field.split_once('=').unwrap();
                (name.trim(), value.trim())
            })
            .collect();
        assert_eq!(
            fields,
            [
                ("start", (offset / 512).to_string().as_str()),
                ("size", (size / 512).to_string().as_str()),
                ("type", kind),
            ],
            "{entry}"
        );
    }
}
