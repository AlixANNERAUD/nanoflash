# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0](https://github.com/AlixANNERAUD/nanoflash/releases/tag/nanoflash-cli-v0.1.0) - 2026-11-23

### Added

- add layout check and layout write to the command line
- add flash, detect, image build and layout check --manifest to the command line
- say what was being done when the serial port fails
- read text, counted wildcards and failed opens in transcripts
- let backends protect the flash below the application and name their checksum
- add SAM-BA to the command line and the Arduino Nano 33 BLE board files
- *(flash)* report the SAMD21 erase to the end of flash as a warning
- read Intel HEX images for every protocol
- add AVR to the command line, and the Uno, Nano, Mega 2560 and Leonardo board files
- build the AVR backend into its own WebAssembly module
- take the arduino and wiring protocol names and check where a hex image starts on the command line
- preview a layout for a flash size from the command line

### Changed

- parse arguments with getargs-derive
- share the role name and read grow as a 64-bit integer

### Fixed

- exit 1 with JSON on argument errors, survive a closed pipe, reject image directories
- keep hardware inputs across checkouts and harden the QEMU script
- reopen the device with the serial number it had
- check every image and erase entry before anything is erased
- skip the verification in secure download mode instead of failing
- detect the chip without changing the baud rate

### Miscellaneous

- set up the workspace, tooling and continuous integration
- give the nanoflash dependency of the command a version
- cover port selection and tighten the command line tests
- flash QEMU in download mode and add hardware-in-the-loop hooks
- move to getargs-derive 0.1.1 and Rust 1.85 for the command line
- flash arduino/BOSSA's emulated SAM-BA bootloader and compare with bossac
- flash the Arduino STK500 bootloaders emulated by simavr
- add the Uno, Nano, Mega 2560 and Leonardo to the hardware in the loop
- read the Nano layout from a fixture inside the library package
- write test files under the target directory instead of the system temporary directory
