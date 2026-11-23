# Prior art

What already exists for flashing and partitioning microcontrollers, natively and in the browser,
and what nanoflash reuses, contributes to or writes itself. Surveyed on 2026-11-22; versions and
licences come from crates.io, npm and the projects' repositories on that date.

## Summary

- No Rust or WebAssembly project unifies several bootloader protocols behind one API, and none
  unifies partition layouts across vendors. The closest, probe-rs, only drives debug probes.
- Every web flasher covers one vendor or one protocol (esptool-js, esp-web-tools, webdfu, DAPjs).
- The building blocks exist and are permissively licensed: `espflash` (without its serial port
  layer), `dfu-core`, `nusb` (now with a WebUSB backend), `mbrman`, `gpt_disk_types`,
  `esp-idf-part`, `uf2_block`.
- Two protocols have no reusable Rust implementation: the Espressif ROM loader over an
  asynchronous transport, and SAM-BA. nanoflash writes those, using `espflash` and BOSSA as
  references.

## Multi-protocol flashers

| Project | Scope | Licence | Notes |
| --- | --- | --- | --- |
| [probe-rs](https://github.com/probe-rs/probe-rs) 0.32.0 | Debug probes: CMSIS-DAP, ST-Link, J-Link, ESP USB-JTAG, WCH-Link | MIT OR Apache-2.0 | No ROM serial loaders, SAM-BA, DFU or UF2. Synchronous; the draft [async/WebUSB PR #2958](https://github.com/probe-rs/probe-rs/pull/2958) has been stale since 2025-05. The CMSIS-DAP layer is not a separate crate. |
| [esptool-js](https://github.com/espressif/esptool-js) 0.7.1 | Espressif ROM loader over Web Serial (and WebUSB for CH34x bridges) | Apache-2.0 | Espressif only. |
| [esp-web-tools](https://github.com/esphome/esp-web-tools) 10.5.2 | Install button and manifest on top of esptool-js | Apache-2.0 | Espressif only. Its manifest format is accepted by nanoflash. |
| [Adafruit WebSerial ESPTool](https://github.com/adafruit/Adafruit_WebSerial_ESPTool) | Espressif over Web Serial | MIT | Espressif only. |
| [webdfu](https://github.com/devanlai/webdfu) | USB DFU over WebUSB | ISC | Unmaintained since 2021. |
| [DAPjs](https://github.com/ARMmbed/dapjs) | CMSIS-DAP over WebUSB | MIT | JavaScript. |
| [Arduino Cloud Agent](https://github.com/arduino/arduino-create-agent) | Native helper that runs bossac, avrdude... for the browser | AGPL-3.0 | Not a web flasher; not reused. |
| [espflash-wasm](https://github.com/georgik/espflash-wasm) | Experiment running espflash in the browser | MIT OR Apache-2.0 | Last change in 2023. |
| [ESP32 Bit Pirate web tools](https://geo-tp.github.io/ESP32-Bit-Pirate/web-tools/) | esptool-js, STM32 AN3155, avrdude in WebAssembly | various | Several separate tools on one page, not one API. |

## Per protocol

### Espressif ROM serial bootloader

- [espflash](https://github.com/esp-rs/espflash) 4.7.0, MIT OR Apache-2.0 (relicensed in
  [#224](https://github.com/esp-rs/espflash/issues/224)). Supports ESP32, C2, C3, C5, C6, C61, H2,
  H21, H4, P4, S2, S3 and S31.
  - Its `Connection` and `Flasher` are synchronous and bound to the concrete `serialport` types;
    they only exist behind the `serialport` feature. There is no custom transport and Web Serial
    is asynchronous only, so they cannot run in the browser.
  - Without that feature the crate still provides the `command` module (encoding of every loader
    command), `target::Chip` (chip detection values, register addresses, write sizes, eFuse
    fields) and the flasher stubs. **We built it for `wasm32-unknown-unknown` with
    `default-features = false`: it compiles.** Its dependencies (`regex`, `deku`, `object`,
    `esp-idf-part`) weigh on the WebAssembly bundle; this is measured before committing to it.
  - [Issue #1091](https://github.com/esp-rs/espflash/issues/1091) asks for user-supplied serial
    ports; the maintainer put it on hold but would consider a contribution for a future major
    version. [PR #834](https://github.com/esp-rs/espflash/pull/834) (no_std with `embedded-io`)
    is open and inactive since 2025-07.
- [esptool](https://github.com/espressif/esptool) 5.6.0 (Python) is **GPL-2.0-or-later**:
  nanoflash does not port from it.
- [esp-flasher-stub](https://github.com/espressif/esp-flasher-stub) is Apache-2.0 OR MIT; the
  stubs can be redistributed.
- The protocol is documented by Espressif:
  [serial protocol](https://docs.espressif.com/projects/esptool/en/latest/esp32/advanced-topics/serial-protocol.html).

**Decision:** write our own asynchronous loader over nanoflash's transport trait: SLIP framing,
commands, reset sequences, orchestration, written from Espressif's protocol documentation with
`espflash` and esptool-js as references. Copy the few chip constants we need (detection values,
about 40 lines) from `espflash` with attribution (MIT OR Apache-2.0) rather than depending on it:
its dependency tree would dominate the WebAssembly bundle for those few lines. Use the ROM loader
without a flasher stub at first (the ROM supports compressed writes and MD5); the stub is a later
speed-up. Offer the transport design upstream on #1091 once it has proven itself.

### SAM-BA and the bossac dialect

- [BOSSA](https://github.com/shumatech/BOSSA) 1.9.1 and the active
  [arduino/BOSSA](https://github.com/arduino/BOSSA) fork (1.9.1-arduino6): **BSD-3-Clause**.
  The reference implementation is
  [`Samba.cpp`](https://github.com/arduino/BOSSA/blob/master/src/Samba.cpp): ASCII commands
  `N#`, `V#`, `O/o/W/w/S/R/G`, plus the Arduino extensions advertised as `[Arduino:XYZ...]` in
  the version string: `X` (erase), `Y` (write buffer to flash), `Z` (CRC), `I` (identify chip),
  `K` (reset). arduino/BOSSA ships a SAM-BA emulator in its tests.
- The SAMD bootloader of
  [ArduinoCore-samd](https://github.com/arduino/ArduinoCore-samd/tree/master/bootloaders/zero) is
  LGPL-2.1 (device side; not vendored).
- **Arduino Nano 33 BLE** ([ArduinoCore-mbed boards.txt](https://github.com/arduino/ArduinoCore-mbed/blob/main/boards.txt)):
  `upload.tool=bossac`, `use_1200bps_touch=true`, `wait_for_upload_port=true`, VID `0x2341`,
  PIDs `0x005a`, `0x805a`, `0x015a`, `0x025a`; upload command `bossac -U -i -e -w file.bin -R`.
  arduino/BOSSA identifies `nRF52840-QIAA` through `I`. The bootloader itself is distributed as a
  binary only ([issue #90](https://github.com/arduino/ArduinoCore-mbed/issues/90)).
- Rust: only [`bossa`](https://github.com/kiibohd/BOSSA), C++ bindings through `cxx`, native
  only, last changed in 2023.

**Decision:** write our own SAM-BA implementation in Rust from the protocol (the command set
with the Arduino extensions, the nRF52 and SAMD families of Arduino's bootloaders, and the
1200-baud touch), with BOSSA as a behaviour reference only: no code is ported, so the crate stays
MIT OR Apache-2.0. A binding to BOSSA works natively only: its serial I/O is blocking C++ with
exceptions, so running it in the browser would need a separate C++ to WebAssembly build and
Chromium's JSPI to suspend on Web Serial. That would mean a second toolchain and module to replace
about 700 lines of Rust.

### USB DFU

- [dfu-core](https://crates.io/crates/dfu-core) 0.11.1: MIT OR Apache-2.0, `#![no_std]`,
  sans-I/O (`DfuIo` trait), synchronous and `async` modes, DfuSe memory layouts.
- [dfu-nusb](https://crates.io/crates/dfu-nusb) 0.2.0 (async, on nusb 0.2) and
  [dfu-libusb](https://crates.io/crates/dfu-libusb) 0.6.2: same licence.
- [webdfu](https://github.com/devanlai/webdfu): ISC, JavaScript, unmaintained.

**Decision:** reuse `dfu-core` over nanoflash's USB transport (`nusb` natively and in the
browser).

### UF2

- Specification: [microsoft/uf2](https://github.com/microsoft/uf2) (MIT); family identifiers in
  [`uf2families.json`](https://github.com/microsoft/uf2/blob/master/utils/uf2families.json).
  RP2040 `0xe48bff56`, absolute `0xe48bff57`, data `0xe48bff58`, RP2350 Arm secure `0xe48bff59`,
  RP2350 RISC-V `0xe48bff5a`, RP2350 Arm non-secure `0xe48bff5b`; they match
  [pico-sdk `boot/uf2.h`](https://github.com/raspberrypi/pico-sdk/blob/master/src/common/boot_uf2_headers/include/boot/uf2.h).
- Crates: [uf2_block](https://crates.io/crates/uf2_block) 0.1.0 (MIT OR Apache-2.0),
  [uf2-decode](https://crates.io/crates/uf2-decode) 0.2.0 (MIT),
  [uftwo](https://crates.io/crates/uftwo) 0.3.0 (MPL-2.0, not used).
- [picotool](https://github.com/raspberrypi/picotool) 2.3.1: BSD-3-Clause.

**Decision:** encode blocks ourselves (a 32-byte header and a 476-byte payload; about 50 lines);
native
writes copy the file onto the mounted drive, the browser writes through the File System Access
API (a directory picker on the board's drive).

### CMSIS-DAP and Nordic serial DFU (later)

- probe-rs natively. In the browser, DAPjs (MIT) through JavaScript until probe-rs becomes
  asynchronous; contributing to probe-rs #2958 is the long-term route.
- Nordic serial DFU: no Rust implementation surveyed in depth; to be revisited when the backend
  is scheduled.

## Partition tables and file system images

| Need | Crate | Licence | `no_std` |
| --- | --- | --- | --- |
| Espressif partition table | [esp-idf-part](https://crates.io/crates/esp-idf-part) 0.6.0 | MIT OR Apache-2.0 | no (CSV, regex, deku) |
| MBR | [mbrman](https://crates.io/crates/mbrman) 0.7.0 | MIT OR Apache-2.0 | yes (new in 0.7) |
| GPT | [gpt_disk_types](https://crates.io/crates/gpt_disk_types) 0.16.1 | MIT OR Apache-2.0 | yes |
| GPT (reading, tests) | [gpt](https://crates.io/crates/gpt) 4.1.0 | MIT | no |
| FAT image | [fatfs](https://crates.io/crates/fatfs) 0.3.6 | MIT | 0.4 (unreleased) |
| LittleFS image | [littlefs2](https://crates.io/crates/littlefs2) 0.8.1 | MIT OR Apache-2.0 | yes, but wraps C (`littlefs2-sys`) |

`esp-idf-part` needs `std`, and both the Espressif table (32-byte entries plus an MD5 row) and
the MBR (four 16-byte entries) are small fixed formats, so nanoflash writes them itself, `no_std`,
and uses `esp-idf-part`, `mbrman`, ESP-IDF's `gen_esp32part.py` and `sfdisk` in tests to read its
output back. LittleFS image building stays native only (C toolchain); the browser accepts an
image built elsewhere.

## Transports

| Need | Native | Browser |
| --- | --- | --- |
| Serial | [serialport](https://crates.io/crates/serialport) 4.10.1 (MPL-2.0, used as a dependency, never copied) | `web-sys` Web Serial (behind `web_sys_unstable_apis`); no maintained wrapper crate, so nanoflash writes a thin one. serialport's Web Serial work ([#165](https://github.com/serialport/serialport-rs/issues/165), [PR #224](https://github.com/serialport/serialport-rs/pull/224) closed) did not land. |
| USB | [nusb](https://crates.io/crates/nusb) 0.2.9 (Apache-2.0 OR MIT) | nusb's WebUSB backend ([#199](https://github.com/kevinmehall/nusb/pull/199), since 0.2.6, async API only, `--cfg=web_sys_unstable_apis`) |
| Mass storage (UF2) | `std::fs` | File System Access API |

Web Serial and WebUSB are implemented by Chromium-based browsers only; the command line is the
fallback everywhere else.

## esp-web-tools manifest

Documented at [esphome.github.io/esp-web-tools](https://esphome.github.io/esp-web-tools/), types
in [`src/const.ts`](https://github.com/esphome/esp-web-tools/blob/main/src/const.ts):

- top level: `name`, `version`, `home_assistant_domain?`, `funding_url?`,
  `new_install_prompt_erase?`, `new_install_skip_erase?` (deprecated),
  `new_install_improv_wait_time?`, `builds[]`;
- build: `chipFamily` (`ESP32`, `ESP32-C2`, `ESP32-C3`, `ESP32-C5`, `ESP32-C6`, `ESP32-C61`,
  `ESP32-H2`, `ESP32-P4`, `ESP32-S2`, `ESP32-S3`, `ESP8266`), `parts[]` with `path` (relative to
  the manifest, or a URL) and `offset`, and `serialType?` (`cdc` or `uart`, in the code only).

## Licences of reference implementations

| Reference | Licence | How nanoflash uses it |
| --- | --- | --- |
| espflash | MIT OR Apache-2.0 | reference for the loader; chip constants copied with attribution |
| esptool-js | Apache-2.0 | reference for reset sequences in the browser |
| esptool (Python) | GPL-2.0-or-later | **not used** |
| esp-flasher-stub | Apache-2.0 OR MIT | stubs, later |
| BOSSA, arduino/BOSSA | BSD-3-Clause | behaviour reference, no code ported |
| ArduinoCore-samd bootloader | LGPL-2.1 | protocol reference only, not vendored |
| dfu-core | MIT OR Apache-2.0 | dependency |
| microsoft/uf2 | MIT | specification and family identifiers |
| pico-sdk | BSD-3-Clause | family identifiers (cross-check) |
| probe-rs | MIT OR Apache-2.0 | dependency (later) |
| DAPjs | MIT | browser dependency (later) |
| serialport | MPL-2.0 | dependency only |
| Arduino Cloud Agent | AGPL-3.0 | **not used** |

nanoflash itself is licensed MIT OR Apache-2.0.

## What could go upstream

- **espflash:** an asynchronous transport trait (issue #1091), once nanoflash's loop has run on
  hardware. This would let espflash itself run in the browser.
- **probe-rs:** async probes for WebUSB (#2958), when the CMSIS-DAP backend is scheduled.
- **esp-idf-part:** `no_std` support, so the Espressif writer can move next to the others.
