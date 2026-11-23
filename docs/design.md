# nanoflash design

## Intent

Every microcontroller family ships its own flashing tool (esptool or espflash, dfu-util, bossac,
picotool, probe-rs) with its own partition format, and web flashing exists only per vendor.
nanoflash **lays out flash and writes firmware** for boards of any vendor through **one API**,
natively (Rust library and command line) and in the browser (WebAssembly, an npm package and a
drop-in web component).

It is project-agnostic: it knows nothing about any operating system, image format or signing
scheme. Callers bring those through documented hooks.

## Goals, non-goals, anti-goals

Goals:

- One vendor-neutral TOML **layout file** per board: flash size and named partitions with a
  role. nanoflash resolves offsets, checks them, reports headroom and writes each family's table.
- One vendor-neutral **board manifest**: protocol, chip, layout, images, reset sequence, hashes.
  The esp-web-tools manifest is accepted as is.
- One **asynchronous** code path for every backend, running natively and in WebAssembly over a
  transport trait.
- Backends, in order: Espressif ROM loader, SAM-BA (with Arduino's bossac dialect), the
  STK500v1, STK500v2 and AVR109 bootloaders of Arduino's AVR boards, USB DFU, UF2, then
  CMSIS-DAP and Nordic serial DFU.
- Binary and Intel HEX images for every backend.
- A `build` crate so firmware `build.rs` scripts read the same layout file (single source of
  truth).
- CI-friendly command line: clear errors, `--json`, no prompts unless asked.

Non-goals:

- Building firmware images from ELF files, or any vendor image format beyond what flashing needs.
- Signing, encryption, secure boot provisioning (verification is a caller hook).
- A serial monitor or debugger.
- Browsers without Web Serial or WebUSB (Firefox, Safari): the command line is the fallback.

Anti-goals (would make this a failure even if it works):

- Two engines per protocol (one native, one in the browser) that drift apart.
- A WebAssembly bundle that ships every backend to flash one board.
- Code ported from a copyleft implementation (esptool.py is GPL-2.0-or-later).
- Interactive prompts or ambiguous output in CI.

## Constraints

- Stable Rust, edition 2024. `core` is `no_std` + `alloc`. No `unsafe` without a `SAFETY:`
  comment.
- Licence MIT OR Apache-2.0 for every crate. No code is ported from implementations under other
  licences; they serve as behaviour references only.
- Identifiers, documentation and messages spell words out (`device`, `directory`,
  `configuration`) unless an external API forces otherwise.
- Conventional commits; tooling as in the author's other crates: mise, mbx (mr-boxington),
  release-plz, cargo-deny, codecov, a PR title check and the nanobom SBOM upload.
- Web Serial and WebUSB are Chromium-only.
- Futures are not `Send`: browser APIs are single-threaded, so every trait is designed for a
  single-threaded executor.

## Approaches considered

For the backends the choice was between reusing existing tools and writing our own code; see
[`prior-art.md`](prior-art.md) for the survey.

| Approach | Trade-off | Outcome |
| --- | --- | --- |
| **A. Reuse protocol libraries where they run asynchronously over our transport; write the missing loop ourselves** | One engine per protocol on both platforms; some code of our own (Espressif loop, SAM-BA) | **Chosen** |
| B. Wrap the existing tool per platform (espflash natively, esptool-js in the browser; bossac natively) | Least code, but two engines per protocol, tests cover only one, no SAM-BA in the browser | Rejected (anti-goal) |
| B'. For SAM-BA, BOSSA everywhere (`cxx` natively, BOSSA compiled to WebAssembly with Chromium's JSPI in the browser) | Reuses BOSSA, but needs a C++ toolchain, a second WebAssembly module with JavaScript glue, and makes the crate BSD-3-Clause | Rejected by the author in favour of our own code |
| C. Make the upstream tools asynchronous first (espflash, probe-rs), then build on them | Best for the ecosystem, but blocked on maintainers' major-version decisions | Deferred: offered upstream once our loop has run on hardware |

Per backend, choosing for the smallest codebase, simplicity and portability (dependencies count
as code: they weigh on the WebAssembly bundle and on `no_std`):

| Backend or writer | Reused | Ours |
| --- | --- | --- |
| Espressif ROM loader | `miniz_oxide` (compression), `md-5` | SLIP framing, commands, a ~40-line chip table copied from `espflash` (MIT OR Apache-2.0, attributed), asynchronous loop, reset sequences, orchestration. No flasher stub at first: the ROM already supports compressed writes and MD5, the stub is only a speed-up and can come later. The ESP32 is written uncompressed: its ROM programs a whole 32 KiB inflate window after every compressed image (seen in Espressif's QEMU). |
| SAM-BA | none usable in the browser | our own code, written from the protocol (Microchip's SAM-BA documentation, observed bootloader behaviour); BOSSA (BSD-3-Clause) is a behaviour reference only, no code ported |
| STK500v1, STK500v2, AVR109 (Arduino AVR) | none: no crate is `no_std`, asynchronous over our transport and permissively licensed (see [`prior-art.md`](prior-art.md)) | our own code, written from Atmel's application notes AVR061, AVR068 and AVR109 and from observing the bootloaders; avrdude, Optiboot, ATmegaBOOT, the wiring bootloader and Caterina (GPL) are behaviour references only, no code ported |
| Intel HEX | `ihex` as a test-only dependency, to read the fixture back | a decoder of about 300 lines: `ihex` needs `std` |
| Espressif partition table | `md-5` | ~100 lines: 32-byte entries and the MD5 row. `esp-idf-part` (needs `std`, `regex`, `deku`) is a test-only dependency to read our output back. |
| MBR | none | ~50 lines: four 16-byte entries and the signature. `mbrman` is a test-only dependency. |
| USB DFU (later) | `dfu-core` (`no_std`, sans-I/O) | glue to the USB transport, DfuSe |
| UF2 (later) | none (the format is a 32-byte header) | encoder, family identifiers, drive handling |
| CMSIS-DAP (later) | `probe-rs` natively; in the browser, to be decided when scheduled | glue |

`espflash` as a dependency was considered (it builds for WebAssembly without its serial feature),
but it would bring `regex`, `deku`, `object` and `esp-idf-part` into the bundle for about 40 lines
of constants, so the constants are copied instead.

CMSIS-DAP in the browser would today mean DAPjs next to probe-rs, which is two engines (an
anti-goal). The choice between that exception and waiting for an asynchronous probe-rs (or
contributing it) is made when the backend is scheduled.

## Architecture

### Crates

Three crates in one workspace, plus the npm package. Fewer crates means one version to release,
no glue between crates, and features instead of crate boundaries for what a firmware or a web
page leaves out.

| Crate | Role |
| --- | --- |
| `nanoflash` (`crates/nanoflash`) | The library, `no_std` + `alloc` by default. Modules: `layout` (model, resolution, checks, report), `manifest` (board manifest, esp-web-tools conversion), `transport` (traits), `table` (writers), `flash` (orchestration, Intel HEX), `espressif`, `samba`, `avr`, `touch` (the 1200-baud touch SAM-BA and AVR109 share), `build` (constants and `memory.x` for `build.rs`), `native` and `web` (transports), `testing` (transcript replay and recording). |
| `nanoflash-cli` (`crates/nanoflash-cli`) | The `nanoflash` command. |
| `nanoflash-web` (`crates/nanoflash-web`) | `wasm-bindgen` bindings, built once per backend into separate WebAssembly modules. |

Features of `nanoflash` (every one only adds):

| Feature | Enables | Needs `std` |
| --- | --- | --- |
| `toml`, `json` | parsing layouts and manifests | no |
| `espressif`, `samba`, `avr` (STK500v1, STK500v2 and AVR109) | backends | no |
| `table-espressif`, `table-mbr`, `table-linker` | writers | no |
| `build` | helpers for `build.rs` (implies `toml`, `table-linker`) | yes |
| `native` | `serialport` transport, port listing | yes |
| `web` | Web Serial transport over hand-written `wasm-bindgen` bindings (no `web_sys_unstable_apis` flag needed downstream) | no |
| `testing` | transcript replay and recording transports | no |

The USB transport (`nusb`) arrives with the first USB backend (DFU); deliverables 1 to 4 only use
serial ports.

The npm package (`web/` at the repository root) holds the TypeScript API, the backend loader and
the `<nanoflash-button>` web component.

### Layout file

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
size = "remaining"            # shares what is left with the other growing partitions
```

- Sizes and offsets are integers or strings with a unit (`"4 KiB"`, `"8 MiB"`, `"0x1000"`).
- The flash size is a size, or `"detect"` with a `minimum` (`size = "detect"`, `minimum = "4 MiB"`):
  offline tools (`layout check`, `layout write`, `build`) resolve the layout against the minimum,
  and `flash` resolves it again against the flash the chip reports.
- A partition size is a size, `"remaining"`, or a table `{ minimum = "1 MiB", grow = 3, maximum =
  "2 MiB" }`: at least `minimum`, plus a share of the free space weighted by `grow` (a whole number
  from 1, default 1), up to `maximum` (optional). `remaining` is `{ minimum = 0, grow = 1 }`.
- Roles: `bootloader`, `partition-table`, `firmware`, `update` (with `slot = <n>`), `storage`,
  `crash-record`, `custom`. `partition-table` is added to the list of roles from the brief,
  because table writers need to know where their table goes.
- Per-family attributes live in a table named after the family (`espressif`, `mbr`, `gpt`,
  `linker`) and override what the role implies.

**Resolution** keeps the file order. Partitions with a fixed offset stay where they are; the
others go at the first offset after the previous partition that satisfies their alignment.
Resolution and checks fail on: duplicate names, zero sizes, misaligned fixed offsets, overlaps,
exceeding the flash size, a `remaining` left without space, fixed offsets that descend in a
`detect` layout, a `maximum` below its `minimum`, a growing `firmware` or `update` partition, a
partition whose role needs a fixed offset placed after one that grows with the flash, a flash
below the minimum of the layout, roles a writer cannot express. Each error names the partitions and the addresses involved.

**Growth.** Partitions place first at their minimum size; the free space is then shared:

- R1 *Segments.* Every partition with a fixed offset starts a segment of the file order; a segment
  ends where the next one starts, the last one at the end of the flash. Growth only fills the free
  space at the end of its own segment, so only the last segment depends on the flash size. In a
  `detect` layout, fixed offsets ascend in file order, so that no partition lies above the last
  segment as it grows; a layout with a fixed flash size keeps any order.
- R2 *Steps.* The share of a `{ grow }` partition is a multiple of the largest of its alignment
  and the alignments of the partitions after it in its segment, so these move by whole multiples
  of their alignment and need no new padding.
- R3 *`remaining`* grows in steps of the largest alignment of the partitions after it, or of one
  byte when it is the last of its segment: alone at the end of a segment it fills to the byte, as
  before.
- R4 *Sharing.* The free space is shared by weight, each share rounded down to its step; a
  `maximum` holds a share and what it leaves goes to the others; the rounding remainder stays
  unallocated. Several `remaining` partitions share like `{ grow = 1 }`, and the partitions after a
  growing one are pushed towards the end of its segment. Every layout valid before growing sizes
  resolves exactly as before.
- R5 *Roles.* `firmware` and `update` never take the table form (their images are built for a
  size). They may be `remaining` where that size does not depend on the device: a fixed flash
  size, or a segment that ends at a fixed offset. In a `detect` layout, a `bootloader`,
  `partition-table`, `firmware` or `update` partition whose offset would move with the flash is
  refused.
- R6 *Dependence.* In a `detect` layout, a partition *grows with the flash* when it is
  `remaining` or `{ grow }` in the last segment, unless its `maximum` keeps it below one step
  above its minimum; it *moves with the flash* when it is in the last segment, has no fixed offset
  and comes after a partition that grows with the flash.

**Report:** for each partition its offset, end, size and, when images are known (from a
manifest), the image size and the headroom left (bytes and percent); plus the unallocated flash.
An image larger than its partition is an error. Human output is a table; `--json` gives the same
data.

### Table writers

| Writer | Output | Notes |
| --- | --- | --- |
| `espressif` | Binary partition table (ours, checked against `esp-idf-part` and `gen_esp32part.py`), placed at the `partition-table` partition | Role mapping: `firmware` to `app/factory`, `update` slot n to `app/ota_n`, `storage` to `data` with a subtype from `filesystem`, `crash-record` to `data/coredump`; `custom` needs explicit `espressif` attributes. Update slots require an `otadata` partition. |
| `mbr` | 512-byte MBR (ours, checked against `mbrman` and `sfdisk`) | For SD cards and disk images; sector size 512, partition types from attributes or the role |
| `gpt` (later) | Protective MBR, primary and backup GPT (`gpt_disk_types`) | Type GUIDs from attributes; deterministic disk and partition GUIDs from a seed for reproducible images |
| `linker` | `memory.x` regions or Rust constants | For families with no on-flash table (an nRF52840 behind a bootloader whose application starts at a fixed address) |

### File system images

Later, a `FileSystemImageWriter` trait builds an image of a given size from a list of files, with
FAT (`fatfs`) and LittleFS (`littlefs2`, native only) behind features. Nothing depends on it: a manifest image may target a `storage` partition with a caller-supplied image,
which is padded to the partition and checked to fit.

### `build` feature

```rust
// build.rs of a firmware
fn main() {
    nanoflash::build::Layout::read("layouts/board.toml")
        .unwrap()
        .write_constants("layout.rs")   // into OUT_DIR
        .unwrap();
}
```

Generated:

```rust
pub mod application { pub const OFFSET: u32 = 0x10000; pub const SIZE: u32 = 0x300000; }
pub const FLASH_SIZE: u32 = 0x800000;
```

It prints `cargo:rerun-if-changed` for the layout and fails the build with the same errors as
`layout check`. Optionally it writes `memory.x` through the `linker` writer.

In a layout that detects the flash size, the constants and regions state what holds on every
device, so both writers need the layout resolved for its minimum flash and refuse another
(`TableError::NotAtMinimum`); `build` resolves the minimum itself. Only the values written decide
between `u32` and `u64`.

- **R7 Constants.** `FLASH_SIZE` becomes `FLASH_MINIMUM_SIZE`. A partition that grows with the
  flash gets `MINIMUM_SIZE` instead of `SIZE`: its size on the minimum flash, the least it has on
  any device, since shares never shrink when the flash grows. That is the size the author's
  layout gives it on the minimum flash, so it is at least the declared minimum of the layout (the
  `minimum_size` of the report is the declared minimum, `MINIMUM_SIZE` the size on the minimum
  flash). A partition that moves with the flash gets no `OFFSET` and no `ADDRESS`, so a firmware
  that uses them fails to compile instead of writing at the wrong place. A partition that does
  both has `MINIMUM_SIZE` and no `OFFSET`: moving wins for placement, growing for size. Others
  keep `SIZE`, so the constants of a fixed layout are unchanged.
- **R8 Linker regions.** A partition that grows with the flash keeps a region of its size on the
  minimum flash, preceded by a comment: every byte of that region belongs to the partition on
  every device, and the linker never places anything past it. A partition that moves with the
  flash gets no region, a comment says so: no fixed `ORIGIN` is right on every device, and a
  missing region fails the link loudly. This holds for a partition that also grows.

### Transports

```rust
pub trait SerialPort {
    /// Waits up to `timeout`; returns 0 when nothing arrived.
    async fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, TransportError>;
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError>;
    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError>;
    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError>; // DTR, RTS
    async fn sleep(&mut self, duration: Duration);
    fn identity(&self) -> Option<UsbIdentity>; // vendor, product, serial number
}

/// Finds or reopens a port, for resets that re-enumerate (1200-baud touch, USB-Serial-JTAG).
pub trait PortProvider {
    type Port: SerialPort;
    async fn open(&mut self, filter: &DeviceFilter, baud_rate: u32, timeout: Duration) -> Result<Self::Port, TransportError>;
}
```

The brief asked for one transport trait with USB control requests too. USB devices get their own
trait (control and bulk transfers, over `nusb` natively and in the browser) with the first USB
backend, DFU; no device is both a serial port and a raw USB device. A `PortProvider` is added
because the touch reset makes the bootloader appear as a new device. Timeouts are part of `read` so `no_std` backends need no timer of their own.

In the browser, a re-enumerated device is only reachable without a user gesture if permission
was granted earlier. The web component asks for the application and bootloader identities of the
board in one `requestPort` filter list; when the bootloader device was never granted, it shows a
second button ("Select the board in bootloader mode") instead of failing.

### Backends

```rust
pub trait Backend {
    async fn connect(&mut self) -> Result<ChipInformation, Error>;      // reset into bootloader, detect
    async fn erase(&mut self, region: Region) -> Result<(), Error>;
    async fn write(&mut self, segments: &[Segment<'_>], progress: &mut dyn Progress) -> Result<(), Error>;
    async fn verify(&mut self, segments: &[Segment<'_>], progress: &mut dyn Progress) -> Result<(), Error>;
    async fn reset_to_application(&mut self) -> Result<(), Error>;
    // The whole flash in the order `flash` sends it, before the first erase; `Ok` by default.
    fn check_plan(&self, erase_all: Option<Region>, erases: &[Region], segments: &[Segment<'_>]) -> Result<(), Error>;
}
```

The facade dispatches over an enum of the enabled backends (`async fn` in traits is not
object-safe). Each backend detects the chip when the protocol allows, checks it against the
manifest, writes, verifies (MD5 on Espressif, CRC through `Z` on SAM-BA, read-back otherwise, as on AVR),
and resets into the application.

### What `flash` writes and erases (builder's calls, to be confirmed)

- It writes exactly the images the manifest lists, plus the partition table generated from the
  layout when the layout has a `partition-table` partition and no manifest image targets it. A
  manifest image targeting that partition wins, and `layout check` warns when it differs from the
  generated table; in a `detect` layout where partitions grow or move with the flash, such an
  image is refused (`partition-table-image-for-detected-flash`): no fixed table fits every chip.
- *Resolution at connect.* Before connecting, a `detect` layout is resolved for its minimum and
  every input is checked against it, image sizes included: an image larger than its partition on
  the minimum is refused, even for a chip with more flash. Every prepared image remembers how it
  was placed (by partition, by offset, or the generated table), and every erase entry the
  partition it names. Right after `connect`, before any check against the chip and any erase,
  the layout is resolved again for the flash the chip reports (R10): a chip below the minimum
  fails with `flash-too-small`; a chip that reports no size (secure download mode, an unknown
  flash identifier) keeps the minimum with a `flash-size-not-detected` warning; a chip of exactly
  the minimum changes nothing; a larger chip moves the images and erase entries that name a
  partition to where it now is, generates the partition table again for that size, and runs the
  checks of the plan again. Then come the checks against the chip (`writable_from`,
  `writable_to`, erases to the end of flash) and the backend's `check_plan`, on the new places.
- *Inputs tied to an offset* (R9). An image or an erase entry given by offset, and an Intel HEX
  image (its addresses are in the file, even when the manifest names its partition), may not
  overlap a partition that moves with the flash (`fixed-offset-on-moving-partition`): it would
  stay where the partition was on the minimum. This is checked before connecting against the
  minimum, and again after resolving for the chip, since a partition that moves by a whole step
  can cover what was the unallocated end of the flash on the minimum. Images and erase entries
  that name a partition follow it.
- *Warnings of a `detect` layout.* `partitions-grew` names each partition larger than on the
  minimum, with both sizes; on the Espressif protocol it adds that the bootloader image must be
  built for that flash size or with flash size detection, since ESP-IDF applications use the
  flash size of the bootloader header, which nanoflash does not patch. `partitions-moved` names
  each partition that starts elsewhere than on the minimum, whose data written by a firmware for
  another flash size is not where the layout now puts it. `flash-size-not-detected` says the
  minimum was kept. Each is also sent as a progress message when it happens. The report lists the
  warnings of preparing the images, then these, then `erases-to-end`, then
  `not-verified-in-secure-download-mode`.
- *The bootloaders and a `detect` layout.* The layout describes the whole flash the chip
  reports. On AVR that size includes the bootloader at the top, so a partition that grows to the
  end of the flash covers it: an erase entry or an image that reaches into it is refused by the
  `writable_to` check, before any erase. On the SAMD21, erase entries must end at the end of
  flash: a partition that grows in steps or up to a `maximum` may stop short of it on a larger
  chip, and an erase entry for it is then refused before any erase; a lone `remaining` at the end
  of the layout fills to the end of the flash on every chip.
- It erases only the regions it writes (rounded to the erase unit), except on the SAMD21 (see
  below). `--erase-all` (and `eraseAll: true` in the JavaScript API) erases the flash first, from
  the lowest offset the bootloader writes (`writable_from`, which `connect` reports) to the end:
  a bootloader the chip protects, such as SAM-BA's, is never erased. Data partitions such as NVS
  or storage are not touched unless an image or an erase entry targets them, again except on the
  SAMD21.
- On SAM-BA, Arduino's `X` command depends on the chip:
  - nRF52840 (Nano 33 BLE): `X` erases nothing; each write erases the 4 KiB pages it programs,
    and an erase entry or `--erase-all` writes `0xFF` over its pages.
  - SAMD21: `X` erases from its address to the end of flash. Writing erases from the lowest
    written address to the end of flash, so everything stored after the lowest image, data
    partitions included, is gone; nanoflash says so in its progress messages and adds an
    `erases-to-end` warning to the report, naming the partitions of the layout it erased that no
    image is written to. Erase entries must end at the end of flash, and images are written in
    order of offset.
- On AVR (STK500v1, STK500v2, AVR109), the bootloaders have no usable chip erase (Optiboot
  ignores it, Arduino never sends it): "erased" means "programmed with `0xFF`". Optiboot,
  ATmegaBOOT and Caterina erase each page as they program it; an erase entry or `--erase-all`
  programs `0xFF` over its pages. The bootloader sits at the top of the flash, which `connect`
  reports as `writable_to` (`0x7E00` on an Uno with Optiboot): nothing at or above it is
  written, and `--erase-all` stops at `writable_to`, or at the bootloader partition of the
  layout above the application when it starts lower (the Nano's layout keeps 2 KiB for the old
  bootloader, while Optiboot takes 512 bytes).
- The wiring bootloader of the Mega 2560 (STK500v2) erases one page per page programmed, at an
  erase address of its own that starts at 0x0 and moves one page on each time, wherever the
  programmed page goes, and erases nothing from its `APP_END`, 0x3C000 (16 KiB under the end of
  flash, twice its 8 KiB). So `writable_to` is 0x3C000, and before the first erase or write the
  backend replays that address over the whole flash (`--erase-all`, the erase entries, then the
  images in order of offset): images must start at 0x0 and follow each other, and an erase entry
  must start at 0x0, unless `--erase-all` erased every page first. A flash that breaks the order
  is refused before anything is written.
- It does not initialise `otadata` and does not patch the Espressif bootloader header (flash
  size, mode, frequency): both are image formats, which nanoflash leaves to the caller. A
  manifest that wants `otadata` reset lists an image for it, or an `erase` entry for its
  partition.

### Choosing a backend before detection

- The protocol comes from the manifest. One manifest may mix protocols (for example an ESP32-S3
  and an nRF52840 variant of the same product); the USB identity of the selected port, matched
  against each board's `application-devices` and `bootloader-devices`, picks the board first;
  chip detection then picks among the boards of that protocol.
- Ports that identify nothing (USB to UART bridges such as CP210x or CH34x) are matched against
  the boards whose reset sequence fits a bridge (`espressif-classic`); when several protocols
  remain, `flash` stops with an error listing them, and `--board-identifier` settles it.
- `detect()` without a manifest uses a built-in table of known identities (Espressif `0x303a`,
  Arduino `0x2341`, Raspberry Pi `0x2e8a`, ST `0x0483` in DFU mode...) to pick the backend, and
  requires `--protocol` otherwise. It never probes several protocols on the same port, because
  each probe resets the board differently.
- Arduino's AVR boards share the vendors of its SAM-BA boards, so the table lists their products
  from Arduino's `boards.txt` before the vendor rule: the Uno (`2341:0043`, `2341:0001`,
  `2a03:0043`, `2341:0243`, `2341:006a`) speaks STK500v1, the Mega 2560 (`2341:0010`,
  `2341:0042` and their `2a03` and `02xx` variants) STK500v2, and the Leonardo, Micro and other
  ATmega32U4 boards (`2341:8036` with the sketch, `2341:0036` in Caterina, and their kin, with
  Adafruit's and SparkFun's) AVR109. A Nano or a Pro Mini sits behind a serial bridge (FTDI,
  CH340) that tells nothing: the manifest's board without devices, or `--protocol`, decides. The
  protocols may be named `arduino` (STK500v1) and `wiring` (STK500v2), as in `boards.txt`.
- In the browser, only the module of the chosen backend is loaded, so the 250 KiB budget holds
  for `detect()` too.

Native transports wrap `serialport`'s blocking calls in `async fn`s: the command line runs one
flash at a time on a single-threaded executor, where this is harmless. Library users embedding
nanoflash in a multi-threaded runtime run it on a dedicated thread (documented).

### Board manifest

TOML or JSON (same schema through `serde`):

```toml
# boards/arduino-nano-33-ble.toml
[[board]]
identifier = "arduino-nano-33-ble"
name = "Arduino Nano 33 BLE"
protocol = "samba"
chip = "nrf52840"
layout = "../layouts/nano33ble.toml"
reset = "touch-1200"
application-devices = [{ vendor = 0x2341, product = 0x805a }]
bootloader-devices = [{ vendor = 0x2341, product = 0x005a }]

[[board.image]]
partition = "application"          # or offset = 0x10000
path = "firmware.bin"              # relative to the manifest, or a URL
sha256 = "9f86d0..."               # optional
```

- A manifest may list several boards; when it does, `flash` picks the one whose chip matches
  what the backend detects (as esp-web-tools does with `chipFamily`), or the one named by
  `--board-identifier`.
- Reset sequences are named: `espressif-classic` (DTR/RTS through a UART bridge),
  `espressif-usb-serial-jtag`, `touch-1200`, `uf2-touch-1200`, `none`, or `custom` with a list of
  steps (`{ dtr = true, rts = false, wait = "100 ms" }`).
- An **esp-web-tools manifest** (JSON with `builds[].chipFamily`) is detected and converted on
  load: protocol `espressif`, chip from `chipFamily`, images from `parts` at their offsets, no
  layout (offsets only), reset chosen from `serialType` or the detected port. Only the fields
  that affect flashing are honoured; `new_install_prompt_erase` maps to the erase option of the
  web component. Improv Wi-Fi provisioning and `home_assistant_domain` are ignored (no serial
  console, see non-goals). `ESP8266` builds are rejected with a clear error: ESP8266 is not in
  scope and espflash no longer supports it.
- Hooks supplied by the caller: `ImageSource` (fetch bytes; files natively, `fetch` in the
  browser), `ImageVerifier` (called after SHA-256, for signatures or anything else),
  `Progress` (events: stage, bytes done and total).
- **Intel HEX images** (`.hex`, `.ihex`, by extension) are decoded in `prepare` for every protocol,
  after the SHA-256 check and the verifier, which see the file as read. The image is the span from
  the lowest to the highest address, `0xFF` in the gaps. Its lowest address is an address of the
  memory map: it must equal the offset of the image plus the `base-address` of the flash of the
  layout. `.eep` files (EEPROM) are refused.

### Command line

```
nanoflash layout check <layout> [--manifest <manifest>] [--flash-size <size>] [--json]
nanoflash layout write <layout> --format espressif|mbr|gpt|linker --output <file> [--flash-size <size>]
nanoflash image build --board <manifest> [--board-identifier <id>] [--flash-size <size>] --output image.bin
nanoflash flash --board <manifest> [--port <port>] [--board-identifier <id>] [--no-verify] [--json]
nanoflash detect --port <port> [--protocol <protocol>] [--json]
```

- `image build` merges the images into one flat file padded with `0xFF` (or the layout's disk
  image for MBR and GPT). For a `detect` layout it builds for the minimum flash size, since a
  merged image cannot follow the chip, and `--fill` pads to that size; `--flash-size` builds for
  another size (table, image offsets and padding).
- `--flash-size` resolves a `detect` layout for that size (an error on a fixed layout or below
  the minimum). The `check` report adds the `minimum` and `growth` columns when a partition
  grows, and a line naming the partitions that move with the flash size. Images are always
  checked against the sizes on the minimum flash, which is what `flash` enforces before
  connecting, so `--flash-size` changes the offsets, sizes and headroom shown, not what is
  accepted.
- Errors go to standard error with the cause chain and a hint; exit codes: 0 success, 1 user or
  input error, 2 device or transport error, 3 verification failure.
- `--json` prints one JSON document on standard output (and progress as JSON lines on standard
  error with `--progress json`).
- No prompts; `--port` is required when more than one candidate port exists.
- A hidden `--record <file>` flag records a transcript from real hardware for the test suite.

### Web layer

- `nanoflash-web` builds one WebAssembly module per backend (feature per backend, built by
  `wasm-bindgen-cli`), each containing `core` and that backend only.
- The npm package `nanoflash` exposes `prepare(manifestUrl)`, whose `flash(options)` asks for the
  port inside the click, `detect()`, layout functions, and `loadBackend(protocol)`, which imports
  the right module dynamically once the board is known.
- `<nanoflash-button manifest="manifest.json">`: a custom element with progress, error and
  unsupported-browser states, styled through CSS custom properties and parts.
- Budget: the core and one backend stay under 250 KiB of gzipped WebAssembly; CI measures each
  module and fails above the budget.
- UF2 in the browser writes through the File System Access API: the user picks the board's drive
  once.

## Testing

- **Transcripts:** a text format, one event per line: `>` bytes from the host, `<` bytes from
  the device (hexadecimal), `!` line changes, baud rate changes, waits, re-enumerations, `#`
  comments. the `testing` feature provides a replay transport that fails on any divergence, and a
  recording transport behind `--record`.
- **Sources of transcripts:** Espressif's QEMU (ESP32 and ESP32-C3 in download mode) in CI;
  arduino/BOSSA's SAM-BA emulator for SAM-BA; simavr (GPL-3.0, installed or downloaded at test
  time, never committed or distributed) running Optiboot, ATmegaBOOT and the Mega 2560's wiring
  bootloader from ArduinoCore-avr, through `hardware/simavr/run.sh` (`mise run test-simavr`);
  recordings from real boards added as they are made. Hand-written transcripts cover error
  paths (timeouts, bad checksums, wrong chip).
- **Property tests** (`proptest`): resolution never yields overlap or misalignment and keeps
  fixed offsets; every writer round-trips through a parser: `esp-idf-part` and ESP-IDF's
  `gen_esp32part.py` in CI, `mbrman` and `sfdisk --dump`, `gpt` and `sgdisk` for GPT.
- **WebAssembly:** the backends and the core build for `wasm32-unknown-unknown` in CI;
  replay tests run the WebAssembly modules in Node against a transcript-driven Web Serial mock; the hardware check in a browser is in `hardware/README.md`.
- **Hardware in the loop (optional):** `hardware/` describes the boards attached to a runner;
  `mise run test-hardware` flashes, verifies and checks the board booted (a known string on its
  serial port). Tests are `#[ignore]`d unless `NANOFLASH_HARDWARE` is set; a
  `workflow_dispatch` job targets a self-hosted runner with the `nanoflash-hardware` label.

## Deliverables

One pull request each, stacked:

1. This design and the prior art report, licence files.
2. Workspace and tooling; `nanoflash` (layout model, resolution, checks, manifest model,
   traits); the Espressif, MBR and `linker` writers (the `build` feature's `memory.x` and the
   nRF52840 case need the last one); the `build` feature; `nanoflash layout check` and
   `layout write` (its `--format` lists the writers that exist); all tested. GPT and the file
   system images come after deliverable 4.
3. Transports (native and web); the Espressif backend; `flash`, `detect` and `image build`; the
   npm package and web component for Espressif. Flashing a real ESP32-S3 over USB-Serial-JTAG,
   natively then over Web Serial, needs a board attached; until then the pull request states it
   is verified against transcripts and QEMU only.
4. The SAM-BA backend for the Arduino Nano 33 BLE, under the same hardware caveat. Arduino's
   SAMD bootloaders speak the same dialect (`X`, `Y`, `Z`), so their family data comes along,
   tested against transcripts only.
5. The AVR backend (STK500v1, STK500v2, AVR109) for the Arduino Uno, Nano, Mega 2560 and
   Leonardo, with Intel HEX images for every protocol and an `avr` WebAssembly module; tested
   against transcripts and simavr, with the boards in the hardware in the loop.

Afterwards, in this order unless reprioritised: GPT writer and file system images, USB DFU, UF2,
CMSIS-DAP, Nordic serial DFU.

## Left to the builder

- Exact names and shapes of error types, progress events and JSON output fields.
- Whether `uf2_block` is used or replaced by a few lines of our own.
- The MSRV: the lowest stable version all dependencies accept, checked in CI.
- The transcript file extension and the precise grammar, within the format above.
