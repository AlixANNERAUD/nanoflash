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
import { flash, isSupported, NanoflashError } from "nanoflash";

button.addEventListener("click", async () => {
  if (!isSupported()) return showMessage("Use a Chromium based browser, or the command line.");
  try {
    const report = await flash("/boards/esp32-s3.json", {
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

`flash` asks the user to choose a port, filtered by the devices of the manifest, then flashes.
The port is requested before any other `await`, so call `flash` straight from the click handler
(a browser grants the request to a user gesture only).

To show something before the click, such as the erase checkbox, `prepare` the manifest first:

```ts
import { prepare } from "nanoflash";

const prepared = await prepare("/boards/esp32-s3.json"); // starts loading the WebAssembly module
if (prepared.promptErase) showEraseCheckbox();
button.onclick = () => prepared.flash({ eraseAll: checkbox.checked });
```

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
`details`, `during` and the `raw` object of the library. A WebAssembly trap is fatal: the port is
closed, the module is marked broken (it cannot be instantiated again) and every later call that
needs it fails at once with kind `module-broken` (class `device`). Reload the page.

## Component

`nanoflash/button` registers a `<nanoflash-button>` element that wraps this API. Importing
`nanoflash` does not register it.

## Bundlers

The WebAssembly modules ship in `dist/wasm` and are loaded relative to the module that imports
them (`new URL("...wasm", import.meta.url)`). Each protocol is a separate dynamic import, so a
bundler loads only the module of the board that is flashed. If your bundler does not copy `.wasm`
files it finds this way, copy `node_modules/nanoflash/dist/wasm` next to the chunk.

## Licence

MIT OR Apache-2.0.
