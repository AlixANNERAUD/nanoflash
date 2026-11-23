// The WebAssembly modules over a transcript-driven Web Serial mock.

import assert from "node:assert/strict";
import { posix } from "node:path";
import { afterEach, test } from "node:test";

import { installNavigator, loadModule, repositoryBytes, repositoryFile } from "./environment.mjs";
import { mockPort } from "./mock-serial.mjs";

const TESTS = "crates/nanoflash/tests";

function transcript(name, protocol = "espressif") {
  return repositoryFile(`${TESTS}/transcripts/${protocol}/${name}.transcript`);
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

/** The ports of `connect-reopen`: before the device leaves, and after it comes back. */
function reopening() {
  const [before, after] = transcript("connect-reopen").split("! reopen 303a:1001\n");
  // The reset into the application that ends a detection.
  const reset = "! signals dtr=0 rts=0\n! sleep 100\n! signals dtr=0 rts=1\n! sleep 100\n! signals dtr=0 rts=0\n";
  return { left: mockPort(before), back: mockPort(`! identity 303a:1001\n${after}${reset}`) };
}

test("reopening skips the granted ports that are still open", async () => {
  const { detect } = await loadModule("espressif");
  const { left, back } = reopening();
  let opens = 0;
  const open = left.port.open;
  left.port.open = (options) => {
    opens += 1;
    return open(options);
  };
  // The port the device left is still open, and still listed.
  restoreNavigator = installNavigator([left.port, back.port]);
  const chip = await detect(left.port, JSON.stringify({ protocol: null }), () => {});
  assert.equal(chip.chip, "esp32-s3");
  back.finish();
  assert.equal(opens, 1);
});

test("reopening prefers the ports granted after the reset", async () => {
  const { detect } = await loadModule("espressif");
  const { left, back } = reopening();
  // Another board of the same model, granted before, which must not be talked to.
  const other = mockPort("! identity 303a:1001\n");
  restoreNavigator = installNavigator([]);
  let listings = 0;
  navigator.serial.getPorts = async () => (listings++ === 0 ? [other.port] : [other.port, back.port]);
  const chip = await detect(left.port, JSON.stringify({ protocol: null }), () => {});
  assert.equal(chip.chip, "esp32-s3");
  back.finish();
  assert.deepEqual(other.port.log, []);
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

/**
 * The ports of `flash-nano33ble`: the sketch, up to the touch, and the bootloader the board comes
 * back as.
 */
function nano33ble() {
  const [sketch, bootloader] = transcript("flash-nano33ble", "samba").split("! reopen 2341:005a\n");
  return { sketch: mockPort(sketch), bootloader: mockPort(`! identity 2341:005a\n${bootloader}`) };
}

test("the mock refuses failed opens and malformed lines", () => {
  assert.throws(
    () => mockPort(transcript("connect-not-granted", "samba")),
    /line 5: `! open-fails not-granted` is not reproducible in a browser/,
  );
  assert.throws(() => mockPort('> "N#\n'), /line 1: a quoted text is not closed/);
  assert.throws(() => mockPort('> "N#"X\n'), /line 1: a quoted text must be followed by a space/);
  assert.throws(() => mockPort('> "\\q"\n'), /line 1: `\\q` is not an escape/);
  assert.throws(() => mockPort("> ..0\n"), /line 1: `..0` is not a count/);
  assert.throws(() => mockPort("< ..2\n"), /line 1: `..` is only allowed in a `>` line/);
});

test("the mock follows quoted text and counted wildcards", async () => {
  const { port, finish } = mockPort('> "S#" ..3 01\n< "a b\\n"\n');
  await port.open({ baudRate: 115200 });
  const writer = port.writable.getWriter();
  await writer.write(new Uint8Array([0x53, 0x23, 0xff, 0x01, 0x02, 0x01]));
  writer.releaseLock();
  const reader = port.readable.getReader();
  const { value } = await reader.read();
  reader.releaseLock();
  assert.deepEqual([...value], [0x61, 0x20, 0x62, 0x0a]);
  finish();
});

test("flashes a Nano 33 BLE through the touch", async () => {
  const { flash } = await loadModule("samba");
  const { sketch, bootloader } = nano33ble();
  restoreNavigator = installNavigator([bootloader.port]);
  const events = [];
  const report = await flash(
    repositoryFile(`${TESTS}/boards/nano33ble.json`),
    sketch.port,
    FLASH_OPTIONS,
    (path) => boardFile(path),
    (event) => events.push(event),
  );
  assert.equal(report.board, "arduino-nano-33-ble");
  assert.equal(report.chip.chip, "nrf52840");
  assert.equal(report.chip.protocol, "samba");
  assert.equal(report.chip.writable_from, 0x10000);
  assert.equal(report.chip.erases_to_end, false);
  assert.equal(report.verified, true);
  assert.deepEqual(
    report.images.map((image) => [image.label, image.offset, image.size]),
    [["application", 0x10000, 8192]],
  );
  assert.ok(events.some((event) => event.stage === "writing"));
  // The touch: 1200 baud, then DTR cleared (the transcript checks `setSignals`).
  sketch.finish();
  bootloader.finish();
  assert.deepEqual(sketch.port.log, ["open 115200", "close", "open 1200", "close"]);
  assert.equal(sketch.port.isOpen, false);
  assert.deepEqual(bootloader.port.log, ["open 115200", "close"]);
});

test("warns that a SAMD21 board was erased to the end of flash", async () => {
  const { flash } = await loadModule("samba");
  const [sketch, bootloader] = transcript("flash-mkrwifi1010", "samba").split(
    "! reopen 2341:0054\n",
  );
  const application = mockPort(sketch);
  const samd21 = mockPort(`! identity 2341:0054\n${bootloader}`);
  restoreNavigator = installNavigator([samd21.port]);
  const manifest = JSON.stringify({
    board: [
      {
        identifier: "arduino-mkr-wifi-1010",
        protocol: "samba",
        chip: "samd21g18a",
        reset: "touch-1200",
        "application-devices": [{ vendor: "0x2341", product: "0x8054" }],
        "bootloader-devices": [{ vendor: "0x2341", product: "0x0054" }],
        image: [{ offset: "0x2000", path: "../images/application.bin" }],
      },
    ],
  });
  const report = await flash(
    manifest,
    application.port,
    FLASH_OPTIONS,
    (path) => boardFile(path),
    () => {},
  );
  assert.equal(report.chip.erases_to_end, true);
  assert.deepEqual(report.warnings, [
    {
      kind: "erases-to-end",
      from: 0x2000,
      to: 0x40000,
      partitions: [],
      message:
        "the bootloader erased the flash from 0x2000 to its end at 0x40000 before writing, so what was stored there besides the images is gone",
    },
  ]);
  application.finish();
  samd21.finish();
});

test("the bootloader port must be granted", async () => {
  // Waits the 15 s the backend gives the bootloader to appear before it says the page has no
  // permission for it.
  const { flash } = await loadModule("samba");
  const { sketch } = nano33ble();
  restoreNavigator = installNavigator([]);
  const error = await rejection(
    flash(
      repositoryFile(`${TESTS}/boards/nano33ble.json`),
      sketch.port,
      FLASH_OPTIONS,
      (path) => boardFile(path),
      () => {},
    ),
  );
  assert.equal(error.kind, "transport");
  assert.equal(error.error.kind, "not-granted");
  assert.equal(error.error.filter, "2341:005a");
  assert.equal(error.class, "device");
  sketch.finish();
  assert.equal(sketch.port.isOpen, false);
});

/**
 * The ports of `flash-leonardo`: the sketch, up to the touch, and Caterina, which the board comes
 * back as.
 */
function leonardo() {
  const [sketch, bootloader] = transcript("flash-leonardo", "avr").split("! reopen 2341:0036\n");
  return { sketch: mockPort(sketch), bootloader: mockPort(`! identity 2341:0036\n${bootloader}`) };
}

test("flashes an Uno from an Intel HEX image", async () => {
  const { flash } = await loadModule("avr");
  const { port, finish } = mockPort(transcript("flash-uno", "avr"));
  const events = [];
  const report = await flash(
    repositoryFile(`${TESTS}/boards/uno.json`),
    port,
    FLASH_OPTIONS,
    (path) => boardFile(path),
    (event) => events.push(event),
  );
  assert.equal(report.board, "arduino-uno-r3");
  assert.equal(report.chip.chip, "atmega328p");
  assert.equal(report.chip.protocol, "stk500v1");
  assert.equal(report.chip.writable_to, 0x7e00);
  assert.equal(report.chip.writable_from, null);
  assert.equal(report.chip.erases_to_end, false);
  assert.equal(report.verified, true);
  assert.deepEqual(
    report.images.map((image) => [image.label, image.offset, image.size]),
    [["application", 0, 300]],
  );
  assert.ok(events.some((event) => event.stage === "writing"));
  // The DTR pulse: the transcript checks that DTR is cleared, then set (`setSignals`).
  finish();
  assert.deepEqual(port.log, ["open 115200", "close", "open 115200", "close"]);
  assert.equal(port.isOpen, false);
});

test("flashes a Leonardo through the touch", async () => {
  const { flash } = await loadModule("avr");
  const { sketch, bootloader } = leonardo();
  restoreNavigator = installNavigator([bootloader.port]);
  const report = await flash(
    repositoryFile(`${TESTS}/boards/leonardo.json`),
    sketch.port,
    FLASH_OPTIONS,
    (path) => boardFile(path),
    () => {},
  );
  assert.equal(report.board, "arduino-leonardo");
  assert.equal(report.chip.chip, "atmega32u4");
  assert.equal(report.chip.protocol, "avr109");
  assert.equal(report.chip.writable_to, 0x7000);
  assert.equal(report.verified, true);
  // The touch: 1200 baud, then DTR cleared (the transcript checks `setSignals`).
  sketch.finish();
  bootloader.finish();
  assert.deepEqual(sketch.port.log, ["open 115200", "close", "open 1200", "close"]);
  // Caterina's port is opened at its 57600 baud (a USB port ignores it).
  assert.deepEqual(bootloader.port.log, ["open 57600", "close"]);
  assert.equal(sketch.port.isOpen, false);
  assert.equal(bootloader.port.isOpen, false);
});

test("a read-back difference is a verification error", async () => {
  const { flash } = await loadModule("avr");
  const { port, finish } = mockPort(transcript("flash-uno-read-back", "avr"));
  const error = await rejection(
    flash(
      repositoryFile(`${TESTS}/boards/uno.json`),
      port,
      FLASH_OPTIONS,
      (path) => boardFile(path),
      () => {},
    ),
  );
  assert.equal(error.kind, "read-back-differs");
  assert.equal(error.class, "verification");
  assert.match(error.message, /0x85/);
  finish();
  assert.equal(port.isOpen, false);
});

test("the avr module speaks the three AVR protocols, and only them", async () => {
  const { detect } = await loadModule("avr");
  const { port } = mockPort("");
  const error = await rejection(detect(port, JSON.stringify({ protocol: "samba" }), () => {}));
  assert.equal(error.kind, "protocol-not-enabled");
  assert.equal(error.class, "input");
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

test("checks a detect layout for a flash size", async () => {
  const { checkLayout, writeLayout } = await loadModule("core");
  const text = repositoryFile(`${TESTS}/layouts/esp32-s3-detect.toml`);
  const name = "esp32-s3-detect.toml";
  const grown = checkLayout(text, "[]", name, "8 MiB");
  assert.equal(grown.flash_size, 8388608);
  assert.equal(grown.minimum_flash_size, 4194304);
  const storage = grown.partitions.find((partition) => partition.name === "storage");
  assert.equal(storage.size, 0x4e0000);
  assert.equal(storage.minimum_size, 0);
  assert.equal(storage.growth, 0x4e0000);
  assert.equal(storage.grows_with_flash, true);
  assert.equal(storage.moves_with_flash, false);
  const minimum = checkLayout(text, "[]", name, undefined);
  assert.equal(minimum.flash_size, 4194304);
  assert.equal(minimum.minimum_flash_size, 4194304);
  // The headroom is shown for the flash size, but images are checked against the minimum.
  const image = (size) => JSON.stringify([{ partition: "storage", size }]);
  const withImage = checkLayout(text, image(0x10000), name, "8 MiB");
  assert.equal(
    withImage.partitions.find((partition) => partition.name === "storage").headroom,
    0x4d0000,
  );
  assert.throws(() => checkLayout(text, image(0x100000), name, "8 MiB"), (error) => {
    assert.equal(error.kind, "layout");
    assert.match(error.message, /storage/);
    return true;
  });
  const fixed = repositoryFile(`${TESTS}/layouts/esp32-s3-8mb.toml`);
  assert.deepEqual(
    writeLayout(text, "espressif", name, "8 MiB"),
    writeLayout(fixed, "espressif", "esp32-s3-8mb.toml"),
  );
  for (const call of [
    () => checkLayout(text, "[]", name, "8MB"),
    () => writeLayout(text, "espressif", name, "8MB"),
  ]) {
    assert.throws(call, (error) => {
      assert.equal(error.kind, "invalid-argument");
      assert.equal(error.class, "input");
      assert.match(error.message, /^the flash size "8MB" is invalid: invalid size or offset "8MB"/);
      return true;
    });
  }
  assert.throws(() => checkLayout(fixed, "[]", "esp32-s3-8mb.toml", "16 MiB"), (error) => {
    assert.equal(error.kind, "layout");
    return true;
  });
});

test("flashes a detect board for the flash of the chip", async () => {
  const { flash } = await loadModule("espressif");
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  const messages = [];
  const report = await flash(
    repositoryFile(`${TESTS}/boards/esp32-s3-detect.json`),
    port,
    FLASH_OPTIONS,
    (path) => boardFile(path),
    (event) => {
      if ("message" in event) messages.push(event.message);
    },
  );
  assert.equal(report.board, "esp32-s3-devkitc-detect");
  assert.equal(report.verified, true);
  const [grew] = report.warnings;
  assert.equal(grew.kind, "partitions-grew");
  assert.equal(grew.flash_size, 8388608);
  assert.equal(grew.minimum_flash_size, 4194304);
  assert.equal(grew.protocol, "espressif");
  assert.deepEqual(grew.partitions, [{ name: "storage", size_at_minimum: 0xe0000, size: 0x4e0000 }]);
  assert.match(grew.message, /storage/);
  assert.ok(messages.includes(grew.message), `${JSON.stringify(messages)}`);
  finish();
  assert.equal(port.isOpen, false);
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
