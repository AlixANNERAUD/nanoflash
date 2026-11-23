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

> **Status:** design stage. See [`docs/design.md`](docs/design.md) and
> [`docs/prior-art.md`](docs/prior-art.md).

## Browser support

Web Serial and WebUSB are implemented by Chromium-based browsers only (Chrome, Edge, Opera...).
Elsewhere, use the `nanoflash` command.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
