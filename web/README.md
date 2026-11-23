# nanoflash

Flash microcontroller boards from a web page, over Web Serial. This is the browser package of
[nanoflash](https://github.com/AlixANNERAUD/nanoflash): the Rust library compiled to
WebAssembly, behind a small TypeScript API with no runtime dependency. It lays out the flash of
a board and writes firmware to it (Espressif chips for now).

## Install

```sh
npm install nanoflash
```

## Browser support

Web Serial exists in Chromium based browsers only (Chrome, Edge, Opera), on a secure origin
(`https:` or `localhost`). `isSupported()` tells whether the browser has it. Elsewhere, use the
`nanoflash` command line, which flashes the same manifests from the computer.

## Flash a board

```ts
import { isSupported, prepare, NanoflashError } from "nanoflash";

if (!isSupported()) showMessage("Use a Chromium based browser, or the command line.");
// Before the click: reads the manifest and starts loading the WebAssembly module.
const prepared = await prepare("/boards/esp32-s3.json");
if (prepared.promptErase) showEraseCheckbox();

button.addEventListener("click", async () => {
  try {
    const report = await prepared.flash({
      eraseAll: checkbox.checked,
      onProgress: ({ stage, done, total }) => showProgress(stage, done, total),
      onMessage: (text) => log(text),
    });
    showMessage(`Flashed ${report.board} (${report.chip.chip}), verified: ${report.verified}`);
  } catch (error) {
    if (error instanceof NanoflashError) showMessage(`${error.kind} (${error.errorClass}): ${error.message}`);
    else throw error;
  }
});
```

`prepared.flash` asks the user to choose a port, filtered by the devices of the manifest, then
flashes. A browser shows the port chooser only during a user gesture, and an `await` may end it:
`prepare` the manifest beforehand, and call `prepared.flash` straight from the click handler,
which asks for the port before its first `await`.

`Prepared` has `manifestUrl`, `protocols`, `filters`, `bootloaderFilters` and `promptErase`.
`FlashOptions` are `port`, `boardIdentifier`, `eraseAll`, `verify` (default `true`), `baudRate`,
`onProgress` and `onMessage`. Paths in the manifest are resolved against the manifest URL.

### Manifests

The browser reads **JSON** manifests: either nanoflash's (the keys of the TOML manifest, with
`board` as an array) or an [esp-web-tools](https://esphome.github.io/esp-web-tools/) manifest.
TOML manifests are for the command line; convert one to JSON with the same keys.

## Detect a chip

```ts
import { detect } from "nanoflash";

const chip = await detect({ protocol: "espressif" });
console.log(chip.chip, chip.flashSize);
```

## Layouts

```ts
import { checkLayout, writeLayout } from "nanoflash";

const text = await (await fetch("/layouts/esp32-s3-8mb.toml")).text();
const report = await checkLayout(text, [{ partition: "application", size: 1_048_576 }]);
const table: Uint8Array = await writeLayout(text, "espressif"); // or "mbr", "linker", "constants"
```

## Errors

Every failure is a `NanoflashError` with `kind` (such as `no-sync`, `verification-failed`,
`not-granted`, `disconnected`), `errorClass` (`input`, `device` or `verification`), `message`,
`details`, `during` and the `raw` object of the library. When the user closes the port chooser,
the kind is `no-port-chosen`; when the browser does not show it at all, `port-request-refused`
(a page in an iframe needs `allow="serial"`, and a permissions policy may block Web Serial).
`not-granted` means that the board came back as a device the page has no permission for (see
the component's bootloader button).

A WebAssembly trap (a bug of nanoflash) is fatal for its module, which cannot be instantiated
again. A layout call runs synchronously and sees the trap at once. `flash` and `detect` run in
microtasks, where a trap is an uncaught exception (the browser logs it) that would leave their
promise pending: while they run, the package listens for uncaught `WebAssembly.RuntimeError`s
(the `error` event of the page, `uncaughtExceptionMonitor` in Node) and rejects the call when one
comes. Either way, the call rejects with kind `module-broken` (class `device`), its port is
closed, and every later call that needs the module fails at once with the same error. Reload the
page.

## Component

`nanoflash/button` registers a `<nanoflash-button>` element that wraps this API (no dependency).
Importing `nanoflash` does not register it.

```html
<script type="module">import "nanoflash/button";</script>
<nanoflash-button manifest="firmware/manifest.json" label="Install"></nanoflash-button>
```

The manifest is ours or an esp-web-tools one, in JSON. Attributes: `manifest`, `label` (default
`Install`) and `erase-all` (erase the whole flash first; without it, the element offers a check
box when the manifest asks for it). The element asks for the port inside the click, shows the
progress and the notes of the library (`onMessage`), the result with what was not verified and
every warning, and the errors (a broken module asks to reload the page), and in a browser
without Web Serial it says so and points to the `nanoflash` command line. When the board comes back
in its bootloader under another USB identity, it shows a second button, "Select the board in
bootloader mode", which asks for that port.

`data-state` on the element is one of `unsupported`, `loading`, `ready`, `connecting`, `erasing`,
`writing`, `verifying`, `resetting`, `done`, `error` and `bootloader-permission`. Events
(bubbling, composed): `nanoflash-progress` (detail: the progress event), `nanoflash-done` (the
report) and `nanoflash-error` (the error). Style it with `--nanoflash-button-background`,
`--nanoflash-button-color`, `--nanoflash-button-radius`, `--nanoflash-progress-color`,
`--nanoflash-font`, and the parts `button`, `erase`, `progress`, `message` and `bootloader-button`.

## Bundlers

The WebAssembly modules ship in `dist/wasm` and are loaded relative to the module that imports
them (`new URL("...wasm", import.meta.url)`). Each protocol is a separate dynamic import, so a
bundler loads only the module of the board that is flashed. If your bundler does not copy `.wasm`
files it finds this way, copy `node_modules/nanoflash/dist/wasm` next to the chunk.

## Licence

MIT OR Apache-2.0.
