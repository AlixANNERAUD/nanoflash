// A trap inside an asynchronous export, which wasm-bindgen runs in a microtask: the trap is an
// uncaught exception there and the promise of the call never settles. In a file of its own: the
// module it breaks stays broken for the whole process.

import assert from "node:assert/strict";
import { test } from "node:test";

import { NanoflashError, detect, loadBackend } from "../dist/index.js";
import { installFetch } from "./environment.mjs";

/** Settles with what `promise` rejected with, or fails when it is still pending after `ms`. */
async function rejectionWithin(promise, ms) {
  let timer;
  const pending = new Promise((resolve) => {
    timer = setTimeout(() => resolve("pending"), ms);
  });
  try {
    const outcome = await Promise.race([promise.then(() => "resolved"), pending]);
    assert.fail(`the call is ${outcome} after ${ms} ms instead of rejecting`);
  } catch (error) {
    if (error instanceof assert.AssertionError) throw error;
    return error;
  } finally {
    clearTimeout(timer);
  }
}

test("an uncaught trap of an asynchronous call rejects it with module-broken", async () => {
  // The module is loaded with fetch, which Node has for file: URLs only through this.
  installFetch();
  const backend = await loadBackend("espressif");
  let calls = 0;
  backend.detect = () => {
    calls += 1;
    queueMicrotask(() => {
      throw new WebAssembly.RuntimeError("unreachable");
    });
    return new Promise(() => {});
  };
  // The test runner fails a test on any uncaught exception: let this one through, and see it.
  const runner = process.rawListeners("uncaughtException");
  process.removeAllListeners("uncaughtException");
  const uncaught = [];
  process.on("uncaughtException", (error) => uncaught.push(error));
  let closed = 0;
  const port = { close: async () => void (closed += 1) };
  let error;
  try {
    error = await rejectionWithin(detect({ protocol: "espressif", port }), 2000);
  } finally {
    process.removeAllListeners("uncaughtException");
    for (const listener of runner) process.on("uncaughtException", listener);
  }
  assert.equal(uncaught.length, 1);
  assert.ok(uncaught[0] instanceof WebAssembly.RuntimeError);
  assert.ok(error instanceof NanoflashError);
  assert.equal(error.kind, "module-broken");
  assert.equal(error.errorClass, "device");
  assert.match(error.message, /unreachable/);
  assert.match(error.message, /reload the page/);
  assert.equal(closed, 1);
  assert.equal(process.listenerCount("uncaughtExceptionMonitor"), 0);

  const untouched = { close: async () => assert.fail("the port must not be touched") };
  assert.strictEqual(await rejectionWithin(detect({ protocol: "espressif", port: untouched }), 2000), error);
  assert.equal(calls, 1);
});
