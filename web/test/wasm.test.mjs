// The WebAssembly modules over a transcript-driven Web Serial mock.

import assert from "node:assert/strict";
import { posix } from "node:path";
import { afterEach, test } from "node:test";

import { installNavigator, loadModule, repositoryBytes, repositoryFile } from "./environment.mjs";
import { mockPort } from "./mock-serial.mjs";

const TESTS = "crates/nanoflash/tests";

function transcript(name) {
  return repositoryFile(`${TESTS}/transcripts/espressif/${name}.transcript`);
}

/** Reads the paths of a manifest of `crates/nanoflash/tests/boards`, as a browser would fetch them. */
function boardFile(path, extra = {}) {
  if (Object.hasOwn(extra, path)) return Promise.resolve(extra[path]);
  return Promise.resolve(repositoryBytes(posix.join(TESTS, "boards", path)));
}

const FLASH_OPTIONS = JSON.stringify({
  board_identifier: null,
  erase_all: false,
  verify: true,
  baud_rate: null,
});

let restoreNavigator = null;

afterEach(() => {
  restoreNavigator?.();
  restoreNavigator = null;
});

/** Runs `promise`, which must reject, and gives what it rejected with. */
async function rejection(promise) {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  assert.fail("expected a rejection");
}

test("detects over a mocked Web Serial port", async () => {
  const { detect } = await loadModule("espressif");
  const { port, finish } = mockPort(transcript("detect-esp32s3-usb"));
  const stages = [];
  const chip = await detect(port, JSON.stringify({ protocol: null }), (event) =>
    stages.push(event.stage),
  );
  assert.equal(chip.chip, "esp32-s3");
  assert.equal(chip.flash_size, 8 * 1024 * 1024);
  assert.deepEqual(chip.usb, { vendor: 0x303a, product: 0x1001, serial_number: null });
  finish();
  assert.deepEqual(port.log, ["open 115200", "close"]);
  assert.equal(port.isOpen, false);
  assert.ok(stages.includes("connecting"));
});

test("flashes the fixture board", async () => {
  const { flash } = await loadModule("espressif");
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  const events = [];
  const report = await flash(
    repositoryFile(`${TESTS}/boards/esp32-s3-usb.json`),
    port,
    FLASH_OPTIONS,
    (path) => boardFile(path),
    (event) => events.push(event),
  );
  assert.equal(report.board, "esp32-s3-devkitc");
  assert.equal(report.verified, true);
  assert.deepEqual(
    report.images.map((image) => [image.label, image.offset, image.size]),
    [
      ["bootloader", 0x0, 4096],
      ["partition-table", 0x8000, 0xc00],
      ["application", 0x20000, 8192],
    ],
  );
  assert.deepEqual(report.warnings, []);
  const writing = events.filter((event) => event.stage === "writing");
  assert.ok(writing.length > 0);
  assert.deepEqual(Object.keys(writing[0]).sort(), ["done", "offset", "stage", "total"]);
  finish();
  assert.equal(port.isOpen, false);
});

test("keeps_leftovers_and_pending_reads", async () => {
  // The boot text, eight SYNC replies and a stale READ_REG reply arrive as one chunk, larger than
  // the 64-byte reads of the backend; the GET_SECURITY_INFO reply comes through the read kept
  // pending after the drain timed out.
  const { detect } = await loadModule("espressif");
  const text =
    transcript("connect-esp32-classic") +
    "! signals dtr=0 rts=1\n! sleep 100\n! signals dtr=0 rts=0\n";
  const { port, finish } = mockPort(text);
  const chip = await detect(port, JSON.stringify({ protocol: "espressif" }), () => {});
  assert.equal(chip.chip, "esp32");
  assert.equal(chip.flash_size, 4 * 1024 * 1024);
  assert.equal(chip.usb, null);
  finish();
  assert.ok(port.chunks[0] > 64, `first chunk of ${port.chunks[0]} bytes`);
  assert.equal(port.isOpen, false);
});

test("reopens the port to change the baud rate", async () => {
  // The ESP32-C3 of Espressif's QEMU, as tests/flash.rs flashes it: through a UART bridge, so
  // the backend switches to 460800 baud.
  const { flash } = await loadModule("espressif");
  const manifest = JSON.stringify({
    board: [
      {
        identifier: "esp32c3-qemu",
        protocol: "espressif",
        chip: "esp32c3",
        layout: "layout.toml",
        reset: "none",
        image: [
          { partition: "bootloader", path: "../images/bootloader.bin" },
          { partition: "application", path: "../images/application.bin" },
        ],
      },
    ],
  });
  const layout = new TextEncoder().encode(
    [
      "[flash]",
      'size = "4 MiB"',
      'alignment = "4 KiB"',
      "",
      "[[partition]]",
      'name = "bootloader"',
      'role = "bootloader"',
      "offset = 0",
      'size = "28 KiB"',
      "",
      "[[partition]]",
      'name = "partition-table"',
      'role = "partition-table"',
      "offset = 0x8000",
      'size = "4 KiB"',
      "",
      "[[partition]]",
      'name = "application"',
      'role = "firmware"',
      "offset = 0x10000",
      'size = "1 MiB"',
      "",
    ].join("\n"),
  );
  const { port, finish } = mockPort(transcript("flash-esp32c3-qemu"));
  const report = await flash(
    manifest,
    port,
    FLASH_OPTIONS,
    (path) => boardFile(path, { "layout.toml": layout }),
    () => {},
  );
  assert.equal(report.verified, true);
  assert.equal(report.chip.flash_size, 4 * 1024 * 1024);
  finish();
  assert.deepEqual(port.log, ["open 115200", "close", "open 460800", "close"]);
  assert.equal(port.isOpen, false);
});

test("a lost device is a device error", async () => {
  // The device leaves during the reset into download mode and the browser has no permission for
  // the device it comes back as.
  restoreNavigator = installNavigator([]);
  const { flash } = await loadModule("espressif");
  const { port } = mockPort(transcript("connect-reopen"));
  const error = await rejection(
    flash(
      repositoryFile(`${TESTS}/boards/esp32-s3-usb.json`),
      port,
      FLASH_OPTIONS,
      (path) => boardFile(path),
      () => {},
    ),
  );
  assert.equal(error.kind, "transport");
  assert.equal(error.error.kind, "not-granted");
  assert.equal(error.error.filter, "303a:1001");
  assert.equal(error.during, "connecting");
  assert.equal(error.class, "device");
  assert.match(error.message, /no permission for a device matching 303a:1001/);
  assert.equal(port.isOpen, false);
});

/**
 * `port`, whose `readable` gives, while `errors` (names of DOMException) last, a stream that
 * fails with the next one, as Web Serial replaces its stream after a break or a framing error.
 */
function withReadErrors(port, errors) {
  const left = [...errors];
  return {
    get readable() {
      const name = left.shift();
      if (name === undefined || port.readable === null) return port.readable;
      return new ReadableStream({
        start(controller) {
          controller.error(new DOMException(`a ${name} on the line`, name));
        },
      });
    },
    get writable() {
      return port.writable;
    },
    get isOpen() {
      return port.isOpen;
    },
    open: (options) => port.open(options),
    close: () => port.close(),
    setSignals: (signals) => port.setSignals(signals),
    getInfo: () => port.getInfo(),
  };
}

test("takes a new reader after a break or a parity error on the line", async () => {
  const { detect } = await loadModule("espressif");
  const { port, finish } = mockPort(transcript("detect-esp32s3-usb"));
  const flaky = withReadErrors(port, ["BreakError", "ParityError"]);
  const chip = await detect(flaky, JSON.stringify({ protocol: null }), () => {});
  assert.equal(chip.chip, "esp32-s3");
  finish();
  assert.equal(port.isOpen, false);
});

test("a line that keeps failing is an io error", async () => {
  const { detect } = await loadModule("espressif");
  const { port } = mockPort(transcript("detect-esp32s3-usb"));
  const failing = withReadErrors(port, Array(8).fill("FramingError"));
  const error = await rejection(detect(failing, JSON.stringify({ protocol: null }), () => {}));
  assert.equal(error.kind, "transport");
  assert.equal(error.error.kind, "io");
  assert.match(error.error.message, /FramingError on the line/);
  assert.equal(port.isOpen, false);
});

test("a write that never completes times out", async () => {
  const { detect } = await loadModule("espressif");
  const { port } = mockPort(transcript("detect-esp32s3-usb"), { stallWrites: true });
  const started = Date.now();
  const error = await rejection(detect(port, JSON.stringify({ protocol: null }), () => {}));
  assert.equal(error.kind, "transport");
  assert.equal(error.error.kind, "io");
  assert.match(error.error.message, /writing to the port did not finish within \d+ ms/);
  assert.equal(error.during, "connecting");
  assert.equal(error.class, "device");
  // The reset into download mode takes about 0.3 s, the write gives up after about 1 s.
  assert.ok(Date.now() - started < 5000, `${Date.now() - started} ms`);
  assert.equal(port.isOpen, false);
});

test("closes the port when an image cannot be read", async () => {
  const { flash } = await loadModule("espressif");
  const { port, finish } = mockPort("! identity 303a:1001\n");
  const error = await rejection(
    flash(
      repositoryFile(`${TESTS}/boards/esp32-s3-usb.json`),
      port,
      FLASH_OPTIONS,
      (path) =>
        path.endsWith("application.bin")
          ? Promise.reject(new Error("HTTP 404"))
          : boardFile(path),
      () => {},
    ),
  );
  assert.equal(error.kind, "image-source");
  assert.equal(error.path, "../images/application.bin");
  assert.equal(error.class, "input");
  assert.match(error.message, /HTTP 404/);
  finish();
  assert.equal(port.isOpen, false);
});

test("refuses unknown options", async () => {
  const { detect } = await loadModule("espressif");
  const { port } = mockPort("");
  const error = await rejection(
    detect(port, JSON.stringify({ protocol: null, baud_rate: 460800 }), () => {}),
  );
  assert.equal(error.kind, "invalid-argument");
  assert.equal(error.class, "input");
  assert.match(error.message, /baud_rate/);
  assert.equal(port.isOpen, false);
});

test("checks a layout with the core module", async () => {
  const core = await loadModule("core");
  const { checkLayout, writeLayout } = core;
  const text = repositoryFile(`${TESTS}/layouts/esp32-s3-8mb.toml`);
  const report = checkLayout(
    text,
    JSON.stringify([{ partition: "application", size: 0x80000 }]),
    "esp32-s3-8mb.toml",
  );
  assert.equal(report.flash_size, 8388608);
  const application = report.partitions.find((partition) => partition.name === "application");
  assert.equal(application.headroom, 0x80000);
  const table = writeLayout(text, "espressif", "esp32-s3-8mb.toml");
  assert.ok(table instanceof Uint8Array);
  assert.equal(table.length, 0xc00);
  const constants = new TextDecoder().decode(writeLayout(text, "constants", "esp32-s3-8mb.toml"));
  assert.match(constants, /pub mod application \{/);
  // The package reads manifests itself: no module exports a manifest reader.
  assert.equal("parseManifest" in core, false);
  assert.equal("parseManifest" in (await loadModule("espressif")), false);
  assert.equal("flash" in core, false);
});

test("layout errors cross as objects with a kind, a message and a class", async () => {
  const { checkLayout, writeLayout } = await loadModule("core");
  const text = repositoryFile(`${TESTS}/layouts/esp32-s3-8mb.toml`);

  const parse = (() => {
    try {
      writeLayout("[flash", "espressif", "broken.toml");
    } catch (error) {
      return error;
    }
  })();
  assert.equal(parse.kind, "layout-file");
  assert.equal(parse.path, "broken.toml");
  assert.equal(parse.class, "input");
  assert.match(parse.message, /broken\.toml/);

  const tooLarge = (() => {
    try {
      checkLayout(text, JSON.stringify([{ partition: "application", size: 0x200000 }]), "a.toml");
    } catch (error) {
      return error;
    }
  })();
  assert.equal(tooLarge.kind, "layout");
  assert.equal(tooLarge.class, "input");
  assert.equal(tooLarge.errors.length, 1);
  assert.match(tooLarge.message, /application/);

  const format = (() => {
    try {
      writeLayout(text, "uf2", "a.toml");
    } catch (error) {
      return error;
    }
  })();
  assert.equal(format.kind, "invalid-argument");
  assert.match(format.message, /uf2/);
});
