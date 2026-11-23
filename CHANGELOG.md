# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0](https://github.com/AlixANNERAUD/nanoflash/releases/tag/v0.1.0) - 2026-11-23

### Added

- add the layout model and its TOML format
- resolve layouts and check alignment, overlap and size
- report the headroom of every partition
- write Espressif partition tables
- write MBR partition tables
- generate linker regions and constants for build scripts
- make Size, ImageSize, Report and ParseError ready for a WebAssembly layer

### Changed

- name the writer in every table error
- parse arguments with getargs-derive

### Documentation

- add the licences, the readme and the simplified crate layout
- describe layout checks, the layout file and the build script in the README

### Fixed

- reject tables ESP-IDF refuses and name the true cause in table errors
- name every overlap and the cause of a failed remaining size, print build errors like the command
- avoid a let chain, which Rust 1.85 does not support

### Miscellaneous

- set up the workspace, tooling and continuous integration
- check resolution on layouts valid by construction
- read the generated tables back with gen_esp32part.py and sfdisk
- test each table feature alone and build with the minimum supported Rust version
