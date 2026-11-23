// The public API of the package, in Node, over the WebAssembly modules and a mocked Web Serial.

import assert from "node:assert/strict";
import { afterEach, beforeEach, test } from "node:test";

import * as nanoflash from "../dist/index.js";
import {
  NanoflashError,
  camelize,
  checkLayout,
  detect,
  fromWasm,
  isSupported,
  loadBackend,
  prepare,
  writeLayout,
} from "../dist/index.js";
import { installFetch, installNavigator, repositoryFile } from "./environment.mjs";
import { mockPort } from "./mock-serial.mjs";

const TESTS = "crates/nanoflash/tests";
const BASE = "https://boards.test/boards";

function transcript(name, protocol = "espressif") {
  return repositoryFile(`${TESTS}/transcripts/${protocol}/${name}.transcript`);
}

let fetchRecord = null;
let restoreNavigator = null;

beforeEach(() => {
  fetchRecord = installFetch();
});

afterEach(() => {
  restoreNavigator?.();
  restoreNavigator = null;
  fetchRecord?.restore();
  fetchRecord = null;
});

/** Prepares the manifest at `url`, then flashes it. */
async function flash(url, options) {
  return (await prepare(url)).flash(options);
}

/** Runs `promise`, which must reject, and gives what it rejected with. */
async function rejection(promise) {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  assert.fail("expected a rejection");
}

test("isSupported follows navigator.serial", () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(globalThis, "navigator", { value: {}, configurable: true });
  try {
    assert.equal(isSupported(), false);
    restoreNavigator = installNavigator([]);
    assert.equal(isSupported(), true);
  } finally {
    restoreNavigator?.();
    restoreNavigator = null;
    if (previous === undefined) delete globalThis.navigator;
    else Object.defineProperty(globalThis, "navigator", previous);
  }
});

test("flashing goes through prepare: there is no flash(manifestUrl)", () => {
  assert.equal("flash" in nanoflash, false);
});

test("prepares our JSON manifest", async () => {
  const prepared = await prepare(`${BASE}/esp32-s3-usb.json`);
  assert.deepEqual(prepared.protocols, ["espressif"]);
  assert.deepEqual(prepared.filters, [{ usbVendorId: 0x303a, usbProductId: 0x1001 }]);
  assert.deepEqual(prepared.bootloaderFilters, [{ usbVendorId: 0x303a, usbProductId: 0x1001 }]);
  assert.equal(prepared.promptErase, false);
  assert.equal(prepared.manifestUrl.href, `${BASE}/esp32-s3-usb.json`);
});

test("prepares an esp-web-tools manifest", async () => {
  const prepared = await prepare(new URL(`${BASE}/esp-web-tools.json`));
  assert.deepEqual(prepared.protocols, ["espressif"]);
  assert.deepEqual(prepared.filters, []);
  assert.deepEqual(prepared.bootloaderFilters, []);
  assert.equal(prepared.promptErase, true);
});

test("flashes with the chosen port", async () => {
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  const requested = [];
  restoreNavigator = installNavigator([port], {
    onRequestPort: (options) => {
      requested.push(options);
      return port;
    },
  });
  const stages = [];
  const report = await flash(`${BASE}/esp32-s3-usb.json`, {
    onProgress: (event) => stages.push(event.stage),
  });
  assert.deepEqual(requested, [{ filters: [{ usbVendorId: 0x303a, usbProductId: 0x1001 }] }]);
  assert.equal(report.board, "esp32-s3-devkitc");
  assert.equal(report.verified, true);
  assert.equal(report.chip.flashSize, 8388608);
  assert.equal(report.chip.secureDownloadMode, false);
  assert.equal(report.chip.writableFrom, null);
  assert.equal(report.chip.erasesToEnd, false);
  assert.deepEqual(report.chip.usb, { vendor: 0x303a, product: 0x1001, serialNumber: null });
  assert.deepEqual(
    report.images.map((image) => image.label),
    ["bootloader", "partition-table", "application"],
  );
  assert.ok(stages.includes("writing"));
  finish();
  assert.equal(port.isOpen, false);
});

test("requests the port before any await of Prepared.flash", async () => {
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  let requested = false;
  restoreNavigator = installNavigator([port], {
    onRequestPort: () => {
      requested = true;
      return port;
    },
  });
  const prepared = await prepare(`${BASE}/esp32-s3-usb.json`);
  const pending = prepared.flash();
  assert.equal(requested, true);
  await pending;
  finish();
});

test("a refused chooser is port-request-refused, a closed one no-port-chosen", async () => {
  restoreNavigator = installNavigator([], {
    onRequestPort: () => {
      throw new DOMException('Access to the feature "serial" is disallowed by permissions policy.', "SecurityError");
    },
  });
  const prepared = await prepare(`${BASE}/esp32-s3-usb.json`);
  const refused = await rejection(prepared.flash());
  assert.ok(refused instanceof NanoflashError);
  assert.equal(refused.kind, "port-request-refused");
  assert.equal(refused.errorClass, "input");
  assert.match(refused.message, /allow="serial"/);
  assert.match(refused.message, /permissions policy/);
  assert.equal(refused.raw.name, "SecurityError");
  restoreNavigator();
  restoreNavigator = installNavigator([]);
  const closed = await rejection(prepared.flash());
  assert.equal(closed.kind, "no-port-chosen");
  assert.equal(closed.errorClass, "input");
});

/** Serves a manifest with an ESP32-S3 board (303a:1001) and an Arduino Nano 33 BLE (SAM-BA). */
function serveMixedManifest() {
  const espressif = JSON.parse(repositoryFile(`${TESTS}/boards/esp32-s3-usb.json`)).board[0];
  const manifest = {
    board: [
      espressif,
      {
        identifier: "arduino-nano-33-ble",
        protocol: "samba",
        chip: "nrf52840",
        reset: "touch-1200",
        "application-devices": [{ vendor: "0x2341", product: "0x805a" }],
        "bootloader-devices": [{ vendor: "0x2341", product: "0x005a" }],
        image: [{ offset: "0x10000", path: "../images/application.bin" }],
      },
    ],
  };
  const served = globalThis.fetch;
  globalThis.fetch = async (input) =>
    String(input) === `${BASE}/mixed.json` ? new Response(JSON.stringify(manifest)) : served(input);
  return `${BASE}/mixed.json`;
}

test("a manifest of several protocols flashes the board the port identifies", async () => {
  const url = serveMixedManifest();
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  restoreNavigator = installNavigator([port]);
  const prepared = await prepare(url);
  assert.deepEqual(prepared.protocols, ["espressif", "samba"]);
  const report = await prepared.flash();
  assert.equal(report.board, "esp32-s3-devkitc");
  assert.equal(report.verified, true);
  finish();
});

test("a manifest of several protocols loads the module of the port it is given", async () => {
  const prepared = await prepare(serveMixedManifest());
  // A Nano 33 BLE in its bootloader: the samba module flashes it, without a touch.
  const bootloader = mockPort(
    transcript("connect-nano33ble-bootloader", "samba") + transcript("write-nano33ble", "samba"),
  );
  const flashed = await prepared.flash({ port: bootloader.port });
  assert.equal(flashed.board, "arduino-nano-33-ble");
  assert.equal(flashed.chip.protocol, "samba");
  assert.equal(flashed.chip.writableFrom, 0x10000);
  assert.equal(flashed.verified, true);
  bootloader.finish();
  const port = (usbVendorId, usbProductId) => ({
    getInfo: () => (usbVendorId === undefined ? {} : { usbVendorId, usbProductId }),
    open: async () => assert.fail("the port must not be opened"),
  });
  const unknown = await rejection(prepared.flash({ port: port(0x1a86, 0x7523) }));
  assert.equal(unknown.kind, "no-matching-board");
  assert.equal(unknown.errorClass, "input");
  assert.match(unknown.message, /1a86:7523/);
  const bridge = await rejection(prepared.flash({ port: port() }));
  assert.equal(bridge.kind, "no-matching-board");
  assert.match(bridge.message, /without a USB identity/);
});

test("uses the port of the options without asking", async () => {
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  restoreNavigator = installNavigator([], {
    onRequestPort: () => assert.fail("requestPort must not be called"),
  });
  const report = await flash(`${BASE}/esp32-s3-usb.json`, { port });
  assert.equal(report.verified, true);
  finish();
});

test("resolves_paths_against_the_manifest", async () => {
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  restoreNavigator = installNavigator([port]);
  await flash(`${BASE}/esp32-s3-usb.json`);
  finish();
  assert.ok(
    fetchRecord.urls.includes("https://boards.test/images/application.bin"),
    fetchRecord.urls.join("\n"),
  );
  assert.ok(fetchRecord.urls.includes("https://boards.test/layouts/esp32-s3-8mb.toml"));
});

test("refuses_toml_manifests", async () => {
  const error = await rejection(prepare(`${BASE}/esp32-s3-usb.toml`));
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.kind, "manifest");
  assert.equal(error.errorClass, "input");
  assert.match(error.message, /JSON/);
  assert.match(error.message, /esp32-s3-usb\.toml/);
});

test("an unreachable manifest names its URL", async () => {
  const url = `${BASE}/missing.json`;
  const error = await rejection(prepare(url));
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.kind, "image-source");
  assert.equal(error.errorClass, "input");
  assert.ok(error.message.includes(url), error.message);
});

test("an unreadable image is an image-source error naming its URL", async () => {
  const { port } = mockPort(transcript("flash-esp32s3-usb"));
  restoreNavigator = installNavigator([port]);
  const real = globalThis.fetch;
  globalThis.fetch = async (input) =>
    String(input).endsWith("application.bin") ? new Response("no", { status: 500 }) : real(input);
  const error = await rejection(flash(`${BASE}/esp32-s3-usb.json`));
  assert.equal(error.kind, "image-source");
  assert.match(error.message, /boards\.test\/images\/application\.bin/);
  assert.equal(port.isOpen, false);
});

test("verification failures keep their class", async () => {
  const { port } = mockPort(transcript("flash-md5-mismatch"));
  restoreNavigator = installNavigator([port]);
  const error = await rejection(flash(`${BASE}/esp32-s3-usb.json`));
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.kind, "verification-failed");
  assert.equal(error.errorClass, "verification");
  assert.equal(port.isOpen, false);
});

test("flashes_twice_in_a_row_after_an_error", async () => {
  const bad = mockPort(transcript("flash-no-sync"));
  restoreNavigator = installNavigator([bad.port]);
  const error = await rejection(flash(`${BASE}/esp32-s3-usb.json`));
  assert.equal(error.kind, "no-sync");
  assert.equal(error.errorClass, "device");
  assert.equal(bad.port.isOpen, false);
  restoreNavigator();

  const good = mockPort(transcript("flash-esp32s3-usb"));
  restoreNavigator = installNavigator([good.port]);
  const report = await flash(`${BASE}/esp32-s3-usb.json`);
  assert.equal(report.verified, true);
  good.finish();
});

test("a lost device is a transport error of the inner kind", async () => {
  const { port } = mockPort(transcript("connect-reopen"));
  restoreNavigator = installNavigator([]);
  const error = await rejection(detect({ protocol: "espressif", port }));
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.errorClass, "device");
  assert.equal(error.kind, "not-granted");
  assert.equal(error.during, "connecting");
});

test("detects the chip", async () => {
  const { port, finish } = mockPort(transcript("detect-esp32s3-usb"));
  const stages = [];
  const chip = await detect({
    protocol: "espressif",
    port,
    onProgress: (event) => stages.push(event.stage),
  });
  assert.equal(chip.chip, "esp32-s3");
  assert.equal(chip.flashSize, 8388608);
  assert.equal(chip.protocol, "espressif");
  assert.deepEqual(chip.usb, { vendor: 0x303a, product: 0x1001, serialNumber: null });
  assert.ok(stages.includes("connecting"));
  finish();
});

test("unknown backends are refused", async () => {
  const error = await rejection(loadBackend("uf2"));
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.kind, "protocol-not-enabled");
  assert.equal(error.errorClass, "input");
});

test("loads each backend once", async () => {
  assert.strictEqual(loadBackend("espressif"), loadBackend("espressif"));
  const backend = await loadBackend("espressif");
  assert.equal(typeof backend.flash, "function");
  assert.equal(typeof backend.detect, "function");
});

test("loads the samba backend", async () => {
  const backend = await loadBackend("samba");
  assert.equal(typeof backend.flash, "function");
  assert.equal(typeof backend.detect, "function");
  assert.notStrictEqual(backend, await loadBackend("espressif"));
});

test("checks a layout", async () => {
  const text = repositoryFile(`${TESTS}/layouts/esp32-s3-8mb.toml`);
  const report = await checkLayout(text);
  assert.equal(report.partitions.length, 8);
  assert.equal(report.flashSize, 8 * 1024 * 1024);
  const application = report.partitions.find((partition) => partition.name === "application");
  assert.ok(application);
  const withImage = await checkLayout(text, [{ partition: "application", size: 1024 }]);
  const checked = withImage.partitions.find((partition) => partition.name === "application");
  assert.equal(checked?.imageSize, 1024);
});

test("a layout error is a NanoflashError", async () => {
  const error = await rejection(checkLayout("this is not toml ="));
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.errorClass, "input");
  assert.equal(error.kind, "layout-file");
});

test("writes a layout", async () => {
  const text = repositoryFile(`${TESTS}/layouts/esp32-s3-8mb.toml`);
  const table = await writeLayout(text, "espressif");
  assert.ok(table instanceof Uint8Array);
  assert.equal(table.length, 0xc00);
  const error = await rejection(writeLayout(text, "nope"));
  assert.equal(error.kind, "invalid-argument");
});

test("fromWasm maps every shape", () => {
  const transport = fromWasm({
    kind: "transport",
    error: { kind: "disconnected" },
    during: "writing",
    class: "device",
    message: "gone",
  });
  assert.equal(transport.kind, "disconnected");
  assert.equal(transport.errorClass, "device");
  assert.equal(transport.during, "writing");
  assert.equal(transport.message, "gone");
  assert.deepEqual(transport.details.error, { kind: "disconnected" });

  const bootMode = fromWasm({
    kind: "not-in-download-mode",
    boot_mode: "0x8",
    class: "device",
    message: "not in download mode",
  });
  assert.deepEqual(bootMode.details, { bootMode: "0x8" });
  const nested = fromWasm({ kind: "image-source", path: "a.bin", error: { serial_number: null } });
  assert.deepEqual(nested.details, { path: "a.bin", error: { serialNumber: null } });

  const thrown = fromWasm(new TypeError("boom"));
  assert.equal(thrown.kind, "internal");
  assert.equal(thrown.errorClass, "device");
  assert.equal(thrown.message, "boom");

  const same = new NanoflashError("x", "input", "y");
  assert.strictEqual(fromWasm(same), same);
  assert.equal(fromWasm("text").kind, "internal");
});

test("camelize converts keys, not values", () => {
  const bytes = new Uint8Array([1]);
  assert.deepEqual(
    camelize({
      flash_size: 1,
      "prompt-erase": true,
      list: [{ serial_number: "a_b" }],
      kind: "partition-table-differs",
      bytes,
    }),
    {
      flashSize: 1,
      promptErase: true,
      list: [{ serialNumber: "a_b" }],
      kind: "partition-table-differs",
      bytes,
    },
  );
});

test("a trap breaks the module: the port is closed and later calls fail fast", async () => {
  const backend = await loadBackend("espressif");
  const original = backend.detect;
  let calls = 0;
  backend.detect = async () => {
    calls += 1;
    throw new WebAssembly.RuntimeError("unreachable");
  };
  try {
    let closed = 0;
    const port = { close: async () => void (closed += 1) };
    const error = await rejection(detect({ protocol: "espressif", port }));
    assert.ok(error instanceof NanoflashError);
    assert.equal(error.kind, "module-broken");
    assert.equal(error.errorClass, "device");
    assert.match(error.message, /reload the page/);
    assert.equal(closed, 1);

    const untouched = {
      close: async () => assert.fail("the port must not be touched"),
    };
    const again = await rejection(detect({ protocol: "espressif", port: untouched }));
    assert.strictEqual(again, error);
    const flashAgain = await rejection(flash(`${BASE}/esp32-s3-usb.json`, { port: untouched }));
    assert.strictEqual(flashAgain, error);
    assert.equal(calls, 1);
  } finally {
    backend.detect = original;
  }
});
