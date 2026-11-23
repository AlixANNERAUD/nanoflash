# Hardware in the loop

Two checks run the flashing path against something other than a transcript: Espressif's QEMU
(no board needed) and real boards attached to a machine.

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
chip of the manifest. Copy transcripts worth keeping into
`crates/nanoflash/tests/transcripts/espressif/recorded/`.

The test is `#[ignore]`d: `cargo test` skips it, and it does nothing unless `NANOFLASH_HARDWARE`
names the boards file.

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
