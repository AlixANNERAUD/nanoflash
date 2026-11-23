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
- add the serial transport traits and transcript replay and recording
- add the native serial transport over serialport
- add the board manifest and read esp-web-tools manifests
- encode the Espressif ROM loader protocol and its chip table
- connect to the Espressif ROM loader and detect the chip
- erase, write compressed, verify and reset Espressif chips
- select the board and prepare the images a manifest flashes
- flash and detect boards through one orchestration for every backend
- add flash, detect, image build and layout check --manifest to the command line
- say what was being done when the serial port fails
- run the Espressif backend in the browser over Web Serial
- read text, counted wildcards and failed opens in transcripts
- let backends protect the flash below the application and name their checksum
- encode SAM-BA commands, Arduino's extensions and the CRC-16 check
- reset Arduino boards into their SAM-BA bootloader and detect the chip
- erase, write, verify and reset through Arduino's SAM-BA bootloaders
- flash and detect Arduino boards over SAM-BA
- build the SAM-BA backend into its own WebAssembly module
- *(flash)* report the SAMD21 erase to the end of flash as a warning
- read Intel HEX images for every protocol
- name the AVR protocols and protect the bootloader at the top of the flash
- encode the STK500v1, STK500v2 and AVR109 commands and the AVR chip table
- reset Arduino AVR boards into their bootloader and detect the chip
- write, read back, erase and reset AVR boards page by page
- flash and detect Arduino AVR boards
- add AVR to the command line, and the Uno, Nano, Mega 2560 and Leonardo board files
- build the AVR backend into its own WebAssembly module
- read growing partition sizes and a detected flash size from layout files
- resolve layouts with growing partitions for any flash size
- report the minimum and the growth of every partition
- give growing partitions a minimum size in constants and linker regions
- resolve the layout again for the flash the chip reports
- preview a layout for a flash size from the command line

### Changed

- name the writer in every table error
- parse arguments with getargs-derive
- write MBR partition tables with mbrman
- share the 1200-baud touch between backends
- share the role name and read grow as a 64-bit integer

### Documentation

- add the licences, the readme and the simplified crate layout
- describe layout checks, the layout file and the build script in the README
- describe flashing, the board manifest and the new modules
- describe the browser package in the README and AGENTS.md
- document the SAM-BA backend and what its erases take
- describe the AVR backend, its bootloaders and their licences
- say bootloaders are built for the flash size, document the report fields of detect layouts

### Fixed

- reject tables ESP-IDF refuses and name the true cause in table errors
- name every overlap and the cause of a failed remaining size, print build errors like the command
- avoid a let chain, which Rust 1.85 does not support
- keep the port identity on Windows and decouple write timeouts
- let the backend choose the reset of esp-web-tools builds without a serial type
- bound each SYNC try to a small byte budget
- build on Rust 1.85 and name the partition in table warnings
- name the image in verification failures and report failed resets
- reopen the device with the serial number it had
- start a new SLIP decoder once the line is quiet after SYNC
- check every image and erase entry before anything is erased
- skip the verification in secure download mode instead of failing
- scale the timeout of each compressed block with what it inflates to
- mark Chip as non-exhaustive, since new Espressif chips will be added
- detect the chip without changing the baud rate
- cap custom reset waits at 10 s and skip a byte order mark in manifests
- write the ESP32 uncompressed, its ROM programs 32 KiB after each compressed image
- bound every Web Serial wait and check each web module by name
- *(web)* take a new reader after a break, framing, parity or overrun error
- *(web)* reopen a device among closed ports, new port objects first
- retry an empty version line and keep the touch for vendor-only bootloader filters
- *(flash)* name the erase entry that stops short of the end of flash, and let empty entries through
- *(samba)* name the reset that ran when no bootloader appears, and refuse commands before connecting
- *(table)* name a partition in the unreachable MBR encoding error
- match the Arduino AVR identities to boards.txt
- follow the Mega 2560 bootloader's sequential erase
- stop Mega 2560 writes where its bootloader stops erasing
- check the Mega 2560's erase order over the whole flash before erasing
- let only a bootloader at the top of the flash shorten --erase-all
- refuse Intel HEX records that cross 64 KiB and allocate fallibly
- tell a misplaced Intel HEX image apart from an invalid one
- wait 5 s for Caterina and tell how to catch it for a first grant
- wait 7 s for Caterina natively and correct the baud-rate note
- require ascending fixed offsets in layouts that detect the flash size
- explain that a flash size needs a layout with size = detect

### Miscellaneous

- set up the workspace, tooling and continuous integration
- check resolution on layouts valid by construction
- read the generated tables back with gen_esp32part.py and sfdisk
- test each table feature alone and build with the minimum supported Rust version
- check the checksum and sequence of every compressed block
- move to getargs-derive 0.1.1 and Rust 1.85 for the command line
- flash arduino/BOSSA's emulated SAM-BA bootloader and compare with bossac
- flash the Arduino STK500 bootloaders emulated by simavr
- read the Nano layout from a fixture inside the library package
- wrap the source notes and doc comments of the AVR backend and the Intel HEX decoder
- write test files under the target directory instead of the system temporary directory
