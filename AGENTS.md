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
| `nanoflash` (`crates/nanoflash`) | The library, `no_std` with `alloc`. Features: `toml`, `table-espressif`, `table-mbr`, `table-linker`, `build`. |
| `nanoflash-cli` (`crates/nanoflash-cli`) | The `nanoflash` command. |

## Invariants

- Licence `MIT OR Apache-2.0` on every crate; authors `Alix ANNERAUD <alix@anneraud.fr>`.
- `crates/nanoflash` is `#![no_std]` with `extern crate alloc`; only the `build` feature enables
  `std`. It must build for `thumbv7em-none-eabihf` and `wasm32-unknown-unknown` with every no_std
  feature.
- `#![forbid(unsafe_code)]` in both crates.
- No panics on any input: checked arithmetic for every offset and size (`u64`), no indexing that
  can fail, no `unwrap` outside tests.
- Spell words out in identifiers, docs and messages (`device`, `directory`, `configuration`),
  unless an external format forces otherwise.
- Never use em or en dashes in any text; use hyphens or commas.
- Conventional commits; no co-author lines.
- Features only add; never gate code on the absence of a feature. `default = []`.
- Every error message names the partitions and addresses involved; addresses print as `0x`
  hexadecimal.

## Before pushing

Run `mise run check` (format, clippy, tests, cross builds) and `mise run deny`.
