# nanoflash

[![crates.io](https://img.shields.io/crates/v/nanoflash.svg)](https://crates.io/crates/nanoflash)
[![docs.rs](https://img.shields.io/docsrs/nanoflash)](https://docs.rs/nanoflash)
[![CI](https://github.com/AlixANNERAUD/nanoflash/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/AlixANNERAUD/nanoflash/actions/workflows/ci.yml)
[![codecov](https://codecov.io/github/AlixANNERAUD/nanoflash/graph/badge.svg)](https://codecov.io/github/AlixANNERAUD/nanoflash)
[![vulnerabilities](https://vulnerabilites.anneraud.fr/badges/864dab79c27abd0a2dea60dda6c2755f.svg)](https://vulnerabilites.anneraud.fr/public/f6b4cbfa68cfc88ebcb7f3b4b9457939)
[![License: MIT OR Apache-2.0](https://img.shields.io/crates/l/nanoflash.svg)](#licence)
[![MSRV](https://img.shields.io/crates/msrv/nanoflash.svg)](https://github.com/AlixANNERAUD/nanoflash/blob/main/crates/nanoflash/Cargo.toml)

Lay out flash and write firmware to microcontroller boards of any vendor, through one API:
natively (Rust library and `nanoflash` command) and in the browser (WebAssembly, an npm package
and a drop-in web component).

- **One layout file** per board (TOML): flash size and named partitions with a role. nanoflash
  computes offsets, checks alignment, overlap and size, reports the headroom of every partition,
  and writes the table each family needs (Espressif partition table, MBR, linker regions).
- **One board manifest**: protocol, chip, layout, images, reset sequence, optional SHA-256.
  Existing [esp-web-tools](https://esphome.github.io/esp-web-tools/) manifests work unchanged.
- **One asynchronous engine per protocol**, the same natively and in the browser: Espressif ROM
  bootloader, SAM-BA (Arduino's bossac dialect), then USB DFU, UF2, CMSIS-DAP and Nordic serial
  DFU.
- **Project-agnostic**: no operating system, image format or signing scheme is built in; callers
  supply them through hooks.

> **Status:** layout checks and the table writers (Espressif, MBR, linker regions and constants)
> are available, in the library and in the `nanoflash` command, and so is flashing Espressif
> chips (ESP32, -S2, -S3, -C2, -C3, -C5, -C6, -H2, -P4) natively through their ROM loader. The
> flashing is verified against recorded transcripts and Espressif's QEMU (ESP32 and ESP32-C3),
> not yet on hardware. The browser package ([`web/`](web/README.md)) flashes the same chips over
> Web Serial, through a TypeScript API and a `<nanoflash-button>` web component, tested in Node
> against a mocked port; it is not published on npm yet. The other protocols come next. See
> [`docs/design.md`](docs/design.md) and [`docs/prior-art.md`](docs/prior-art.md).

## Layout file

```toml
# layouts/esp32-s3-8mb.toml
[flash]
size = "8 MiB"
alignment = "4 KiB"          # default alignment of every partition
base-address = 0x0           # where flash is mapped, for the linker writer

[[partition]]
name = "bootloader"
role = "bootloader"
offset = 0x0
size = "32 KiB"

[[partition]]
name = "partition-table"
role = "partition-table"
offset = 0x8000
size = "4 KiB"

[[partition]]
name = "nvs"
role = "custom"
size = "24 KiB"
espressif = { type = "data", subtype = "nvs" }   # per-family attributes

[[partition]]
name = "application"
role = "firmware"
size = "3 MiB"
alignment = "64 KiB"

[[partition]]
name = "storage"
role = "storage"
filesystem = "littlefs"       # informative; drives the Espressif subtype
size = "remaining"            # at most one partition may fill what is left
```

## Check a layout

```console
$ nanoflash layout check layouts/esp32-s3-8mb.toml
flash 8 MiB (0x800000), unallocated 4 KiB
name             role             offset    end       size      image  headroom
bootloader       bootloader       0x0       0x8000    32 KiB    -      -
partition-table  partition-table  0x8000    0x9000    4 KiB     -      -
nvs              custom           0x9000    0xf000    24 KiB    -      -
application      firmware         0x10000   0x310000  3 MiB     -      -
storage          storage          0x310000  0x800000  5056 KiB  -      -
```

Pass `--image application=firmware.bin` to see the headroom of a partition, and `--json` for
one JSON document. `nanoflash layout write <layout> --format espressif|mbr|linker|constants
--output <file>` writes a table, linker regions or constants.

## Board manifest

A manifest lists the boards a project supports, how to reset them and what to write, in TOML or
in JSON with the same keys:

```toml
# boards/esp32-s3.toml
[[board]]
identifier = "esp32-s3-devkitc"            # selected with --board-identifier
name = "ESP32-S3-DevKitC"
protocol = "espressif"
chip = "esp32-s3"
layout = "../layouts/esp32-s3-8mb.toml"   # paths are relative to the manifest
reset = "espressif-usb-serial-jtag"       # or espressif-classic, none, { custom = [...] }
application-devices = [{ vendor = 0x303a, product = 0x1001 }]

[[board.image]]
partition = "bootloader"                  # or offset = 0x0
path = "../build/bootloader.bin"
sha256 = "d67c656e01756650d77717b0839985a056ec28ffe174601d690fc407a2ceffca"   # optional

[[board.image]]
partition = "application"
path = "../build/application.bin"

[[board.erase]]                           # optional: erased before writing
partition = "nvs"
```

The partition table is generated from the layout and written with the images, unless an image
replaces it. An [esp-web-tools](https://esphome.github.io/esp-web-tools/) manifest (JSON with
`builds` and `parts`) is accepted as is: one board per build, no layout.

Every image and erase entry is read and checked (digest, alignment, overlap, partition size,
flash size, address range) before anything is erased; once connected, they must also fit the
flash the chip reports.

## Flash a board

```console
$ nanoflash flash --board boards/esp32-s3.toml
connecting
writing at 0x0
writing at 0x8000
writing at 0x10000
verifying
resetting
flashed esp32-s3-devkitc (esp32-s3, 8 MiB)
  0x0  bootloader  21 KiB
  0x8000  partition-table  3 KiB
  0x10000  application  812 KiB
verified
```

The port is the only one that suits the board, or `--port`. `--erase-all` erases the whole
flash first, `--no-verify` skips the MD5 checks, `--baud-rate` changes the speed (460800 through
a UART bridge by default), `--json` prints one JSON document. The exit code is 0 on success, 1
for an input error, 2 for a device or connection error and 3 when the flash does not read back
as written.

```console
$ nanoflash detect --port /dev/ttyACM0
protocol espressif
chip esp32-s3
flash 8 MiB
usb 303a:1001
secure download mode no
$ nanoflash image build --board boards/esp32-s3.toml --fill --output flash.bin
wrote 8388608 bytes to flash.bin
```

`nanoflash image build` merges the images of a board into one flash image, `0xFF` between
them, and `nanoflash layout check <layout> --manifest <manifest>` shows the headroom of the
images a manifest writes.

## Use the layout in a firmware

Enable the `build` feature in the build dependencies of the firmware:

```toml
[build-dependencies]
nanoflash = { version = "0.1", features = ["build"] }
```

```rust
// build.rs
fn main() {
    nanoflash::build::generate("layouts/board.toml").unwrap();
}
```

```rust
// src/main.rs: FLASH_SIZE, and a module per partition with OFFSET, SIZE and ADDRESS
include!(concat!(env!("OUT_DIR"), "/nanoflash_layout.rs"));
```

The build script also writes `nanoflash_memory.x`, the `MEMORY` regions of the layout, to
`OUT_DIR` and adds that directory to the linker search path. The firmware's own `memory.x` must
include it:

```
INCLUDE nanoflash_memory.x
```

## Browser support

The browser package flashes over Web Serial, which only Chromium-based browsers implement
(Chrome, Edge, Opera...), on a secure origin (`https:` or `localhost`); so does WebUSB, for the
protocols that will need it. Elsewhere, use the `nanoflash` command, which flashes the same
manifests. See [`web/README.md`](web/README.md) for the package and its component.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
