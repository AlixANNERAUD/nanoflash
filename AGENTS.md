# AGENTS.md

Guidance for coding agents working on this repository. This is the only agent instruction file.

## What this repository is

nanoflash is a vendor-neutral Rust toolkit to lay out flash and write firmware to microcontroller
boards of any vendor, through one API: natively (library and `nanoflash` command) and in the
browser (WebAssembly). One layout file (TOML) per board gives the flash size and the named
partitions with a role; nanoflash computes offsets, checks alignment, overlap and size, reports
the headroom of every partition, and writes the table each family needs. See `README.md` and
`docs/design.md`.

| Crate | Role |
| --- | --- |
| `nanoflash` (`crates/nanoflash`) | The library, `no_std` with `alloc`. Features: `toml`, `json`, `table-espressif`, `table-mbr`, `table-linker`, `flash`, `espressif`, `samba`, `avr`, `testing`, `native`, `build`, `web`. |
| `nanoflash-cli` (`crates/nanoflash-cli`) | The `nanoflash` command: `layout check`, `layout write`, `flash`, `detect`, `image build`. |
| `nanoflash-web` (`crates/nanoflash-web`) | The WebAssembly modules of the browser package (`publish = false`): `core` (layouts), and one module per backend feature (`espressif`, `samba`, `avr`) that adds `flash` and `detect`. |

The browser package is the npm package `nanoflash` in `web/`: TypeScript (`strict`), ES modules,
no runtime dependency, the `<nanoflash-button>` component in `web/src/button.ts`, a demo in
`web/demo`, and Node tests over a mocked Web Serial port in `web/test`. `mise run web-wasm`
builds the modules into `web/src/wasm`.

Modules of the library:

| Module | Role |
| --- | --- |
| `layout`, `table`, `quantity` | Layout files, their resolution and report, the table writers. |
| `manifest` | Board manifests (TOML, JSON) and esp-web-tools manifests (`json`). |
| `transport` | The `SerialPort` and `PortProvider` traits, `DeviceFilter`, transport errors. |
| `backend`, `error` | The `Backend` trait, progress events, the errors and their class (`flash`). |
| `flash` | Board selection, image preparation and checks (Intel HEX images decoded for every protocol), the flash and detect orchestration, `image_build` (`flash`). |
| `espressif` | The ROM loader backend: SLIP, commands, chip table, resets, writes (`espressif`). |
| `touch` | The 1200-baud touch that SAM-BA and AVR109 share, and the bootloader device to wait for after it (`samba` or `avr`). |
| `samba` | The SAM-BA backend of Arduino's bootloaders (Nano 33 BLE, SAMD21 boards): the ASCII commands with Arduino's `X`, `Y` and `Z`, the chip families, writes and CRC-16 checks (`samba`). |
| `avr` | The AVR backend of Arduino's boards: the chip table, the commands of the STK500v1 (Optiboot, ATmegaBOOT), STK500v2 (Mega 2560) and AVR109 (Caterina) bootloaders, page writes and read-back checks, and the Mega 2560's erase order (`avr`). |
| `native` | The host serial port over `serialport` (`native`, needs `std`). |
| `web` | The serial port of the browser over Web Serial, and the provider that finds a device again among the granted ports (`web`, `no_std`, through `wasm-bindgen` and `js-sys`, without `web-sys`). |
| `testing` | Transcript replay and recording, `MemorySource` (`testing`). |
| `build` | Constants and linker regions for a firmware's `build.rs` (`build`, needs `std`). |

Device sessions are tested by replaying transcripts (`crates/nanoflash/tests/transcripts`);
`hardware/qemu/run.sh` flashes Espressif's QEMU, `hardware/samba-emulator/run.sh` flashes
arduino/BOSSA's emulated SAM-BA bootloader with nanoflash and bossac, `hardware/simavr/run.sh`
(`mise run test-simavr`) flashes Optiboot, ATmegaBOOT and the Mega 2560's bootloader running in
simavr (GPL-3.0, fetched into `target/` at test time, never committed) and records their
transcripts, and `hardware/README.md` describes the boards in the loop.

## Invariants

- Licence `MIT OR Apache-2.0` on every crate; authors `Alix ANNERAUD <alix@anneraud.fr>`.
- `crates/nanoflash` is `#![no_std]` with `extern crate alloc`; only the `build` and `native`
  features enable `std`. It must build for `thumbv7em-none-eabihf` and `wasm32-unknown-unknown` with every no_std
  feature.
- `#![forbid(unsafe_code)]` in every crate.
- No panics on any input or device answer: checked arithmetic for every offset and size
  (`u64`), no indexing that can fail, no `unwrap` outside tests, fallible allocation for buffers
  sized by the input.
- Every loop that waits on a device is bounded: attempt counts, inactivity timeouts passed to
  `SerialPort::read` (the library has no clock), byte budgets.
- Nothing is erased before every input is checked: images and erase entries are read and checked
  (digest, alignment, overlap, partition, layout and protocol range) before connecting when the
  board is known, and against the flash the chip reports before the first erase.
- What `connect` reports is checked before the first erase too: every image and erase entry
  starts at or after `writable_from` and ends at or before `writable_to` (an AVR bootloader at
  the top of the flash is never written; `--erase-all` stops there), when the bootloader
  `erases_to_end`, every erase entry ends at the end of flash, and the backend's `check_plan`
  accepts the whole flash in the order it is sent (the Mega 2560's erase order).
- On AVR, "erased" means "programmed with `0xFF`": the bootloaders have no usable chip erase.
- No code from BOSSA, avrdude, Optiboot, Caterina, the Arduino bootloaders or simavr is copied:
  they are a behaviour reference or a test tool only (facts, values and command sequences), and
  every file of `src/avr/` that states such facts says so in its first comment. Bootloader images
  and simavr are fetched into `target/` at test time, never committed.
- Public traits with `async fn` carry `#[allow(async_fn_in_trait)]`; no `Send` bound (browser
  APIs are single-threaded).
- Spell words out in identifiers, docs and messages (`device`, `directory`, `configuration`),
  unless an external format forces otherwise.
- Every WebAssembly module of the browser package is at most 256000 bytes once compressed with
  `gzip -9`; `mise run web-size` (and CI) fails above it.
- Never use em or en dashes in any text; use hyphens or commas.
- Conventional commits; no co-author lines.
- Features only add; never gate code on the absence of a feature. `default = []`.
- Layouts with `size = "detect"`: fixed offsets ascend in file order (only the last segment
  depends on the flash). Offline outputs (`layout check`, constants, linker regions, `build`,
  `image build` without `--flash-size`) resolve at the minimum, and the constants and linker
  writers refuse any other size (`NotAtMinimum`). Before connecting, inputs are checked at the
  minimum (image sizes included). After `connect`, and before any check against the chip or any
  erase, the layout is resolved again for the reported flash, partition-named images and erase
  entries are placed again, the table is regenerated and every plan check runs again. Anything
  tied to an offset (offset images and erases, Intel HEX, a manifest table image) never sits on
  a partition that moves.
- Every error message names the partitions and addresses involved; addresses print as `0x`
  hexadecimal.

## Before pushing

Run `mise run check` (format, clippy, tests, cross builds) and `mise run deny`. After a change to
the browser package, the `web` feature or `nanoflash-web`, also run `mise run web` (builds the
modules, checks their size, type-checks the package and runs its Node tests).
