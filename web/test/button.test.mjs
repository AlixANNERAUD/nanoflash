// <nanoflash-button> in Node, over happy-dom, the WebAssembly modules and a mocked Web Serial.

import assert from "node:assert/strict";
import { afterEach, before, beforeEach, test } from "node:test";

import { GlobalRegistrator } from "@happy-dom/global-registrator";

import { installFetch, installNavigator, repositoryFile } from "./environment.mjs";
import { mockPort } from "./mock-serial.mjs";

const BASE = "https://boards.test/boards";
const NODE_GLOBALS = [
  "ReadableStream",
  "WritableStream",
  "TransformStream",
  "TextEncoder",
  "TextDecoder",
  "Response",
  "Request",
  "Headers",
  "Blob",
];
const MESSAGE_UNSUPPORTED = /Chrome/;

let fetchRecord = null;
let restoreNavigator = null;
let element = null;

before(async () => {
  // The mock port and WebAssembly.instantiateStreaming need Node's own streams and Response.
  const kept = {};
  for (const name of NODE_GLOBALS) kept[name] = globalThis[name];
  GlobalRegistrator.register();
  Object.assign(globalThis, kept);
  await import("../dist/button.js");
});

beforeEach(() => {
  fetchRecord = installFetch();
});

afterEach(() => {
  element?.remove();
  element = null;
  restoreNavigator?.();
  restoreNavigator = null;
  fetchRecord?.restore();
  fetchRecord = null;
});

function transcript(name) {
  return repositoryFile(`crates/nanoflash/tests/transcripts/espressif/${name}.transcript`);
}

function mount(manifest, attributes = {}) {
  element = document.createElement("nanoflash-button");
  element.setAttribute("manifest", `${BASE}/${manifest}`);
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, value);
  document.body.append(element);
  return element;
}

function events(target, ...names) {
  const seen = [];
  for (const name of names) target.addEventListener(name, (event) => seen.push(event));
  return seen;
}

async function until(predicate, what) {
  for (let attempt = 0; attempt < 600; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`timed out waiting for ${what} (state ${element.dataset.state}: ${element.shadowRoot.querySelector("p").textContent})`);
}

const state = () => element.dataset.state;
const part = (name) => element.shadowRoot.querySelector(`[part~="${name}"]`);
const waitFor = (wanted) => until(() => state() === wanted, `the state ${wanted}`);

test("unsupported without Web Serial", async () => {
  mount("esp32-s3-usb.json");
  await waitFor("unsupported");
  assert.match(part("message").textContent, MESSAGE_UNSUPPORTED);
  assert.equal(part("button").disabled, true);
});

test("ready with its label", async () => {
  restoreNavigator = installNavigator([]);
  mount("esp32-s3-usb.json", { label: "Flash it" });
  await waitFor("ready");
  assert.equal(part("button").textContent, "Flash it");
  assert.equal(part("button").disabled, false);
  assert.equal(part("bootloader-button").hidden, true);
  assert.equal(part("progress").hidden, true);
});

test("offers to erase when the manifest asks", async () => {
  restoreNavigator = installNavigator([]);
  mount("esp-web-tools.json");
  await waitFor("ready");
  assert.equal(part("erase").hidden, false);
  element.remove();
  mount("esp-web-tools.json", { "erase-all": "" });
  await waitFor("ready");
  assert.equal(part("erase").hidden, true);
  element.remove();
  mount("esp32-s3-usb.json");
  await waitFor("ready");
  assert.equal(part("erase").hidden, true);
});

test("flashes and reports done", async () => {
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  restoreNavigator = installNavigator([port], { onRequestPort: () => port });
  mount("esp32-s3-usb.json");
  const seen = events(element, "nanoflash-progress", "nanoflash-done", "nanoflash-error");
  await waitFor("ready");
  part("button").click();
  await waitFor("done");
  finish();
  assert.match(part("message").textContent, /^Done: esp32-s3-devkitc was written and restarted\.$/);
  const progress = seen.filter((event) => event.type === "nanoflash-progress");
  assert.ok(progress.some((event) => event.detail.stage === "writing"));
  assert.ok(progress.every((event) => event.bubbles && event.composed));
  const done = seen.filter((event) => event.type === "nanoflash-done");
  assert.equal(done.length, 1);
  assert.equal(done[0].detail.verified, true);
  assert.equal(seen.filter((event) => event.type === "nanoflash-error").length, 0);
});

test("requests the port before any await", async () => {
  const { port, finish } = mockPort(transcript("flash-esp32s3-usb"));
  let requestedAt = null;
  restoreNavigator = installNavigator([port], {
    onRequestPort: () => {
      requestedAt = fetchRecord.calls;
      return port;
    },
  });
  mount("esp32-s3-usb.json");
  await waitFor("ready");
  const before = fetchRecord.calls;
  part("button").click();
  assert.equal(requestedAt, before);
  await waitFor("done");
  finish();
});

test("a closed chooser returns to ready", async () => {
  restoreNavigator = installNavigator([]);
  mount("esp32-s3-usb.json");
  const failures = events(element, "nanoflash-error");
  await waitFor("ready");
  part("button").click();
  await new Promise((resolve) => setTimeout(resolve, 50));
  await waitFor("ready");
  assert.equal(part("message").textContent, "");
  assert.equal(failures.length, 0);
});

test("asks for the bootloader port when it was never granted", async () => {
  const lost = mockPort(transcript("connect-reopen"));
  const good = mockPort(transcript("flash-esp32s3-usb"));
  const requested = [];
  restoreNavigator = installNavigator([], {
    onRequestPort: (options) => {
      requested.push(options);
      return requested.length === 1 ? lost.port : good.port;
    },
  });
  mount("esp32-s3-usb.json");
  await waitFor("ready");
  part("button").click();
  await waitFor("bootloader-permission");
  assert.equal(part("bootloader-button").hidden, false);
  part("bootloader-button").click();
  await waitFor("done");
  good.finish();
  assert.deepEqual(requested[1], { filters: [{ usbVendorId: 0x303a, usbProductId: 0x1001 }] });
});

test("shows errors", async () => {
  restoreNavigator = installNavigator([]);
  mount("missing.json");
  const failures = events(element, "nanoflash-error");
  await waitFor("error");
  assert.match(part("message").textContent, /missing\.json/);
  assert.equal(failures.length, 1);
});
