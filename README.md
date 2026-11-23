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
  bootloader, SAM-BA (Arduino's bossac dialect), the STK500v1, STK500v2 and AVR109 bootloaders
  of Arduino's AVR boards, then USB DFU, UF2, CMSIS-DAP and Nordic serial DFU.
- **Binary and Intel HEX images** (`.hex`, as Arduino, PlatformIO and `objcopy` write them), for
  every protocol.
- **Project-agnostic**: no operating system, image format or signing scheme is built in; callers
  supply them through hooks.

> **Status:** layout checks and the table writers (Espressif, MBR, linker regions and constants)
> are available, in the library and in the `nanoflash` command, and so is flashing, natively and
> in the browser: Espressif chips (ESP32, -S2, -S3, -C2, -C3, -C5, -C6, -H2, -P4) through their
> ROM loader, through Arduino's SAM-BA bootloaders the Arduino Nano 33 BLE (nRF52840) and the
> Arduino SAMD21 boards (such as the MKR WiFi 1010), and through their STK500 and AVR109
> bootloaders Arduino's AVR boards: the Uno, Nano and Pro Mini (ATmega328P, STK500v1), the Mega
> 2560 (STK500v2) and the Leonardo, Micro and Pro Micro (ATmega32U4, AVR109). The flashing is
> verified against recorded transcripts, Espressif's QEMU (ESP32 and ESP32-C3), arduino/BOSSA's
> emulated SAM-BA bootloader and the Arduino STK500 bootloaders emulated by simavr. The browser
> package ([`web/`](web/README.md)) flashes the same chips over Web Serial, through a TypeScript
> API and a `<nanoflash-button>` web component, tested in Node against a mocked port; it is not
> published on npm yet. The other protocols come next. See
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
size = "remaining"            # shares what is left; see growing sizes below
```

A layout can also take the flash size from the chip and let partitions grow with it:

```toml
[flash]
size = "detect"                # read from the chip when flashing
minimum = "4 MiB"              # what offline tools resolve against

# in a partition: at least 1 MiB, three shares of the free space, at most 8 MiB
size = { minimum = "1 MiB", grow = 3, maximum = "8 MiB" }
```

Partitions that grow share the free space by weight (`remaining` is `{ minimum = 0, grow = 1 }`),
in steps that keep the partitions after them aligned. With `size = "detect"`, `flash` resolves the
layout again for the flash the chip reports and writes the partition table of that size.

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

A layout with `size = "detect"` is checked for its minimum flash size. To preview a larger chip,
pass `--flash-size` (to `layout check` and `layout write`; a fixed layout refuses it, and so does
a size below the minimum). The report then shows the minimum and the growth of the partitions
that grow, and lists the partitions that move with the flash size. Images are still checked
against the partition sizes on the minimum flash, as `flash` does, so `--flash-size` changes
only the offsets, sizes and headroom shown:

```console
$ nanoflash layout check layouts/esp32-s3-detect.toml --flash-size "8 MiB"
flash 8 MiB (0x800000), minimum 4 MiB (0x400000), unallocated 60 KiB
name             role             offset    end       size      minimum  growth     image  headroom
...
storage          storage          0x320000  0x800000  4992 KiB  0 B      +4992 KiB  -      -
```

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

A board behind Arduino's SAM-BA bootloader flashes the same way: nanoflash touches the sketch's
port at 1200 baud, waits for the bootloader's port, writes and checks the images with CRC-16, then
resets into the application. Its `chip` is `nrf52840` (Nano 33 BLE), or `samd21g18a`,
`samd21j18a` or `samd21e18a` (Arduino SAMD21 boards); the bootloader at the start of flash is
never written.

```console
$ nanoflash flash --board hardware/nano33ble/manifest.toml
connecting
writing at 0x10000
verifying
resetting
flashed arduino-nano-33-ble (nrf52840, 1 MiB)
  0x10000  application  84 KiB
verified
```

The SAMD21 bootloader erases from the lowest image to the end of flash before writing: what was
stored after it is gone, and nanoflash warns about it, naming the partitions it took.

Arduino's AVR boards take the `.hex` the Arduino IDE builds. The protocol is `stk500v1` (Uno,
Nano, Pro Mini: a pulse on DTR resets them into Optiboot or the older ATmegaBOOT), `stk500v2`
(Mega 2560, its wiring bootloader) or `avr109` (Leonardo, Micro, Pro Micro: a 1200-baud touch
starts Caterina, which shows up on its own port); manifests may also name the first two
`arduino` and `wiring`, as Arduino's `boards.txt` does. Pages are written whole and read back
to check them; the bootloader at the top of the flash is never written.

```console
$ nanoflash flash --board hardware/uno/manifest.toml
connecting
writing at 0x0
verifying
resetting
flashed arduino-uno-r3 (atmega328p, 32 KiB)
  0x0  application  300 B
verified
```

The Mega 2560's bootloader erases one page per page programmed, in order from 0x0, and never
above 0x3C000: its images must start at 0x0 and follow each other, unless `--erase-all` erased
every page first.

The port is the only one that suits the board, or `--port`. `--erase-all` erases the flash
first, from the lowest offset the bootloader writes to the end (a protected bootloader stays; on
AVR, pages are programmed with `0xFF` up to where the bootloader starts), `--no-verify` skips
the MD5, CRC-16 or read-back checks, `--baud-rate` changes the speed (460800 through a UART
bridge by default; the rate of the bootloader on STK500, where STK500v1 tries 115200 then 57600 and STK500v2 uses 115200 only; the
SAM-BA and AVR109 bootloaders ignore it), `--json` prints one JSON document.
The exit code is 0 on success, 1 for an input error, 2 for a device or connection error and 3
when the flash does not read back as written.

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

A board whose layout has `size = "detect"` is checked against the minimum of the layout before
connecting, image sizes included, then placed again for the flash the chip reports: images and
erase entries that name a partition follow it, the partition table is generated for that size,
and nanoflash warns about what grew or moved. Images and erase entries given by offset, Intel HEX
images and a manifest image of the partition table cannot follow the chip, and are refused when
they touch a partition that moves or, for the table, when any partition grows or moves. On an
Espressif board, build the bootloader for the largest flash or with flash size detection: the
application uses the flash size of the bootloader header.

```console
$ nanoflash flash --board boards/esp32-s3-detect.toml
...
warning: the chip has 0x800000 bytes of flash, more than the minimum of 0x400000 of the layout, so partitions grew: "storage" from 0xe0000 to 0x4e0000 bytes; the bootloader image must be built for this flash size or with flash size detection
flashed esp32-s3-devkitc-detect (esp32-s3, 8 MiB)
```

`nanoflash image build` merges the images of a board into one flash image, `0xFF` between
them, and `nanoflash layout check <layout> --manifest <manifest>` shows the headroom of the
images a manifest writes. For a layout with `size = "detect"`, `image build` builds for the
minimum flash size (a merged image cannot follow the chip), and `--fill` pads to it; pass
`--flash-size "16 MiB"` to build for another size: the partition table is generated for it, the
images go where their partitions then are, and `--fill` pads to it.

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
// (see below for layouts that detect the flash size)
include!(concat!(env!("OUT_DIR"), "/nanoflash_layout.rs"));
```

The build script also writes `nanoflash_memory.x`, the `MEMORY` regions of the layout, to
`OUT_DIR` and adds that directory to the linker search path. The firmware's own `memory.x` must
include it:

```
INCLUDE nanoflash_memory.x
```

With `size = "detect"`, the files describe what the firmware can count on with any flash, so they
are written for the minimum flash of the layout (the build script does it, and the writers refuse
a layout resolved for a larger flash):

- `FLASH_MINIMUM_SIZE` replaces `FLASH_SIZE`.
- A partition that grows with the flash (a `remaining` or `{ grow }` size in the last segment)
  has `MINIMUM_SIZE` instead of `SIZE`: its size on the minimum flash, which is the least it has
  on any device. It is at least the `minimum` its layout declares, and more when the minimum
  flash leaves it room (the report of `layout check` shows the declared minimum, the constants
  the size on the minimum flash). Its linker region has that length.
- A partition that moves with the flash (it comes after a growing partition and has no fixed
  offset) has no `OFFSET` and no `ADDRESS`, and no linker region, because no value is right on
  every device. A firmware that uses them fails to compile or to link instead of writing at the
  wrong place. A partition that does both has `MINIMUM_SIZE` only.
- The other partitions keep `OFFSET`, `SIZE` and `ADDRESS`.

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
