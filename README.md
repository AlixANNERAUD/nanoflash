# nanoflash

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
> are available, in the library and in the `nanoflash` command. Flashing comes next. See
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

Web Serial and WebUSB are implemented by Chromium-based browsers only (Chrome, Edge, Opera...).
Elsewhere, use the `nanoflash` command.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
