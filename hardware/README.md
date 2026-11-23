# Hardware in the loop

Three checks run the flashing path against something other than a transcript: Espressif's QEMU
and arduino/BOSSA's SAM-BA emulator (no board needed), and real boards attached to a machine.

## QEMU

`mise run test-qemu` runs `hardware/qemu/run.sh esp32` and `hardware/qemu/run.sh esp32c3` on an
x86-64 or AArch64 Linux machine with libslirp (Debian: `libslirp0`). For each chip the script

1. downloads Espressif's QEMU release pinned in `QEMU_RELEASE` (from
   https://github.com/espressif/qemu/releases) into `target/qemu/` (or `$QEMU_DIRECTORY`), and
   refuses an archive without the SHA-256 digest pinned in the script,
2. starts it in UART download mode with its serial port on a pseudo-terminal (`-serial pty`),
3. flashes the manifest `hardware/qemu/<chip>.toml` (the fixture images, a 4 MiB layout, no reset)
   and records the traffic: the ESP32 at 115200 baud, since the emulated ESP32 stops answering
   after `CHANGE_BAUDRATE`, the ESP32-C3 at 460800 baud,
4. stops QEMU, builds the expected image with `nanoflash image build --fill` and compares it with
   the emulated flash (`cmp`),
5. replays the recording with `--replay`, which must succeed: the recorder and the replayer agree
   on the traffic of the real ROM.

The `qemu` job of `.github/workflows/ci.yml` runs this and is allowed to fail: it is not a
required check. If the emulated ROM cannot run a step (for example the flash identifier), record
the step here; the backend then falls back as the design says.

When Espressif publishes a new release, update `QEMU_RELEASE`, `QEMU_VERSION` and the four
digests (from the `checksum.sha256` asset of the release) in the script, and check the flags against the `esp32` and `esp32c3` pages of
https://github.com/espressif/esp-toolchain-docs/tree/main/qemu (the strap modes `0x0f` and
`0x02` select the UART download mode).

## SAM-BA emulator

`mise run test-samba-emulator` runs `hardware/samba-emulator/run.sh` on Linux or macOS with git,
python3, make and a C++ compiler (no package to install, no root). The script

1. fetches https://github.com/arduino/BOSSA at the commit pinned in `BOSSA_COMMIT` into
   `target/samba-emulator/BOSSA` (or `$SAMBA_EMULATOR_DIRECTORY/BOSSA`), refuses a checkout at
   another commit, and builds `bossac` (`make` prints `wx-config: not found`: only the graphical
   BOSSA needs wxWidgets). BOSSA is BSD-3-Clause: it is only built and run as a test tool, and
   none of its code is in this repository,
2. runs `hardware/samba-emulator/emulate.py`, which starts BOSSA's emulated Arduino Nano 33 BLE
   bootloader (`tests/samba.py`) on a pseudo-terminal for each scenario and prints `<name> ok`
   or `<name> FAILED: <why>`:
   - `nanoflash`: flashes `hardware/samba-emulator/nano33ble.toml` (the fixture application at
     `0x10000`, no reset, no USB identity) and records the traffic; the flash holds the image and
     `0xFF` elsewhere, the emulator saw `X` first and `K` last,
   - `same-flash-as-bossac`: flashes a 154600-byte image with nanoflash, and with
     `bossac -U -i -e -w -v <image> -R` on a fresh emulator; the images are identical in both
     flashes, and so is everything outside the last page. The rest of the last page differs:
     nanoflash pads it with `0xFF`, bossac with `0x00`,
   - `crc-mismatch`: the emulator corrupts one byte; exit code 3, `verification-failed`, then
     `K`,
   - `no-bootloader`: the emulator never answers; exit code 2, `no-bootloader`, nothing written,
   - `below-the-application`: an image at `0x0`; exit code 1, `below-writable-flash`, nothing
     erased or written,
3. replays the recording with `--replay`, which must succeed.

`crates/nanoflash/tests/transcripts/samba/recorded/emulator-nano33ble.transcript` is a copy of
the recording, replayed by `cargo test`.

The `samba-emulator` job of `.github/workflows/ci.yml` runs this and is allowed to fail: BOSSA is
fetched from GitHub, and the emulator is not the real bootloader. If it fails while nanoflash did
not change, first check whether the emulator or bossac changed. To move to a newer BOSSA, update
`BOSSA_COMMIT` (and the commit named in the first line of the copied transcript), run the task,
read the new `tests/samba.py` for changed commands, then copy
`target/samba-emulator/nano33ble.transcript` over the recorded transcript, keeping its first
comment line.

## Boards

Firmware is never committed. Build ESP-IDF's `hello_world` example for the ESP32-S3:

```sh
cd $IDF_PATH/examples/get-started/hello_world
idf.py set-target esp32s3 build
mkdir -p <repository>/hardware/esp32-s3/firmware
cp build/bootloader/bootloader.bin build/hello_world.bin <repository>/hardware/esp32-s3/firmware/
```

Leave the default console configuration: the USB-Serial-JTAG secondary console must stay enabled,
or the text `Hello world!` never reaches the port. `hardware/*/firmware/` is ignored by git.

On the machine the boards are attached to, copy `hardware/boards.example.toml` to
`hardware/boards.toml` (ignored by git) and fill one `[[board]]` per board: its `name`, the
`manifest` (relative to the file), the serial `port` (the stable
`/dev/serial/by-id/...` path) and the text `expect`ed on the port after a flash. Then run:

```sh
mise run test-hardware
```

For each board the test flashes with `--record target/hardware/<name>-flash.transcript`, checks
the report is verified, reads the port for up to 15 seconds until `expect` appears, and runs
`nanoflash detect` with `--record target/hardware/<name>-detect.transcript`, which must name the
chip of the manifest. Transcripts worth keeping go next to the others in
`crates/nanoflash/tests/transcripts/<protocol>/` (the QEMU recordings are
`espressif/flash-esp32-qemu.transcript` and `espressif/flash-esp32c3-qemu.transcript`; the
emulator's is `samba/recorded/emulator-nano33ble.transcript`).

The test is `#[ignore]`d: `cargo test` skips it, and it does nothing unless `NANOFLASH_HARDWARE`
names the boards file.

### Arduino Nano 33 BLE

Build the hello sketch (it prints `Hello world!` on the USB serial port every 500 ms):

```sh
arduino-cli core install arduino:mbed_nano
arduino-cli compile --fqbn arduino:mbed_nano:nano33ble \
  --output-dir hardware/nano33ble/firmware hardware/nano33ble/hello
```

This gives `hardware/nano33ble/firmware/hello.ino.bin`, linked at `0x10000`, which
`hardware/nano33ble/manifest.toml` flashes. Then add the `arduino-nano-33-ble` entry of
`hardware/boards.example.toml` to `hardware/boards.toml`.

What a flash does: a 1200-baud touch on the sketch (`2341:805a`), up to 15 seconds for the
bootloader (`2341:005a`) to appear, then `N#`, `V#`, `I#`, `X`, buffered `S`/`Y` writes per
4 KiB, `Z` (checksum) per 4 KiB, and `K#` (reset). If no bootloader appears, double-tap the reset
button (the orange LED pulses) and pass the bootloader's port. The test reopens the port for 15
seconds, which covers the sketch coming back after `K#`; its `detect` touches the sketch again.

#### Replacing bossac in nanoglia

nanoglia flashes its Nano 33 BLE in `hardware/tools/nanoglia_hardware/board.py`:

| nanoglia | nanoflash |
| --- | --- |
| `bossac -d --port=<name> -U -i -e -w -v <bin> -R` | `nanoflash flash --board <manifest> --port <port> --json` |
| `bossac -U -R` (`leave_bootloader`) | `nanoflash detect --port <bootloader port> --protocol samba` |

`-i` is bossac's device information print: the output of `flash` and `detect` already names the
chip and the flash. `-e` sends `X` from the application start, which erases nothing on the Nano 33
BLE's bootloader (the writes erase the pages, as the emulator shows); `nanoflash flash` does the
same by default. `--erase-all` is the stricter equivalent: it clears the whole application area
with `0xFF` pages (slower), for when stale bytes after the image must go.

The touch, the wait for the bootloader and the reset happen inside `flash`; the port may be the
runner's, the sketch's or the bootloader's. Exit code 2 replaces "No device found", and exit code
3 a failed verification.

A manifest for the runner firmware lists:

- `application-devices = [{ vendor = 0x1209, product = 0x0001 }]` (its pid.codes identity; its
  firmware enters the bootloader on `baud == 1200 && !dtr`),
- `bootloader-devices = [{ vendor = 0x2341, product = 0x005a }]`,
- an image at `offset = 0x10000` of the `.bin` nanoglia writes next to its ELF.

`layout.toml` gives the same `FLASH` region as nanoglia's `memory.x`, through
`nanoflash layout write --format linker` or the `build` feature's constants.

### Hardware workflow

`.github/workflows/hardware.yml` (`Hardware`, started by hand with `workflow_dispatch`) runs
`mise run test-hardware` on a runner labelled `self-hosted` and `nanoflash-hardware`. Register
the runner on the machine that has the boards, and build the firmware and write
`hardware/boards.toml` in its checkout before a run.

## Browser check

The browser package (`web/`) is checked by hand against a board:

1. Build the firmware as above.
2. `mise run web-demo` serves the repository root on port 8000 and prints
   `http://localhost:8000/web/demo/?manifest=/hardware/esp32-s3/manifest.json` (the manifest
   `hardware/esp32-s3/manifest.json` is the JSON twin of `manifest.toml`); open it in Chrome or Edge.
3. Press Install, pick the `USB JTAG/serial debug unit` port, and wait for `done`.
4. Confirm `Hello world!` with `nanoflash detect --port <port>` and a serial terminal.

For the Nano 33 BLE, build its firmware as above and run `mise run web-demo` with
`?manifest=/hardware/nano33ble/manifest.json` (the JSON twin of its `manifest.toml`), pick the
Nano's port, then answer the second button with the bootloader's port (`2341:005a`) when Chrome
asks. The page asks for it once: Chrome remembers the grant, and the next Install finds the
bootloader by itself.
