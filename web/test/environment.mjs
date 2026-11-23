// What the tests need from a browser: `navigator.serial`, `fetch` and the WebAssembly modules.

import { readFileSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/** The root of the repository. */
export const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

/** The text of a file of the repository, by its path from the root. */
export function repositoryFile(path) {
  return readFileSync(join(repository, path), "utf8");
}

/** The bytes of a file of the repository, by its path from the root. */
export function repositoryBytes(path) {
  return new Uint8Array(readFileSync(join(repository, path)));
}

/**
 * Installs a `navigator.serial` whose `getPorts()` gives `ports` (an array, read at each call)
 * and whose `requestPort(options)` calls `onRequestPort(options)` and gives its result, else the
 * first port. Returns a function that restores the previous `navigator`.
 */
export function installNavigator(ports, { onRequestPort } = {}) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const serial = {
    async getPorts() {
      return [...ports];
    },
    async requestPort(options) {
      if (onRequestPort !== undefined) return onRequestPort(options);
      if (ports.length === 0) {
        throw new DOMException("No port selected by the user.", "NotFoundError");
      }
      return ports[0];
    },
  };
  Object.defineProperty(globalThis, "navigator", {
    value: { serial },
    configurable: true,
    writable: true,
  });
  return () => {
    if (previous === undefined) {
      delete globalThis.navigator;
    } else {
      Object.defineProperty(globalThis, "navigator", previous);
    }
  };
}

/**
 * Replaces `fetch`: `https://boards.test/<path>` is served from `root/<path>` (by default
 * `crates/nanoflash/tests`), `file:` URLs from disk (`.wasm` as `application/wasm`), anything else
 * is a 404. Returns `{ calls, urls, restore }`; `calls` counts every request.
 */
export function installFetch(root = join(repository, "crates", "nanoflash", "tests")) {
  const previous = globalThis.fetch;
  const record = {
    calls: 0,
    urls: [],
    restore() {
      globalThis.fetch = previous;
    },
  };
  globalThis.fetch = async (input) => {
    const url = new URL(typeof input === "string" ? input : (input.url ?? String(input)));
    record.calls += 1;
    record.urls.push(url.href);
    let path = null;
    if (url.protocol === "https:" && url.host === "boards.test") {
      path = join(root, decodeURIComponent(url.pathname));
    } else if (url.protocol === "file:") {
      path = fileURLToPath(url);
    }
    if (path === null) return new Response("not found", { status: 404 });
    let body;
    try {
      body = readFileSync(path);
    } catch {
      return new Response("not found", { status: 404 });
    }
    const type = extname(path) === ".wasm" ? "application/wasm" : "application/octet-stream";
    return new Response(body, { status: 200, headers: { "Content-Type": type } });
  };
  return record;
}

/** Imports `web/src/wasm/nanoflash_<name>.js` and instantiates its WebAssembly synchronously. */
export async function loadModule(name) {
  const directory = join(repository, "web", "src", "wasm");
  const module = await import(pathToFileURL(join(directory, `nanoflash_${name}.js`)).href);
  module.initSync({ module: readFileSync(join(directory, `nanoflash_${name}_bg.wasm`)) });
  return module;
}
