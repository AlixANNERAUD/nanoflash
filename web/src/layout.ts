import { camelize } from "./convert.js";
import { assertUsable, settle } from "./backend.js";
import type { LayoutFormat, LayoutOptions, LayoutReport } from "./types.js";

type CoreModule = typeof import("./wasm/nanoflash_core.js");

let core: Promise<CoreModule> | undefined;

function loadCore(): Promise<CoreModule> {
  if (core === undefined) {
    const promise: Promise<CoreModule> = import("./wasm/nanoflash_core.js").then(
      async (module) => {
        await module.default();
        return module;
      },
    );
    promise.catch(() => {
      if (core === promise) core = undefined;
    });
    core = promise;
  }
  return core;
}

async function run<T>(call: (module: CoreModule) => T): Promise<T> {
  try {
    assertUsable("core");
    return call(await loadCore());
  } catch (error) {
    throw await settle(error, "core");
  }
}

/** The flash size for the module: its text, or `undefined` for the size of the layout. */
function flashSize(options: LayoutOptions): string | undefined {
  return options.flashSize === undefined ? undefined : String(options.flashSize);
}

/**
 * Checks the layout file `text` (TOML), with the sizes of the images that go in its partitions,
 * and tells the space used and left in each. `name` is the file name, for messages. With
 * `options.flashSize`, a layout with `size = "detect"` is shown for that flash; the images are
 * still checked against its minimum, the size `flash` enforces.
 */
export function checkLayout(
  text: string,
  images: { partition: string; size: number }[] = [],
  name = "layout.toml",
  options: LayoutOptions = {},
): Promise<LayoutReport> {
  return run((module) =>
    camelize<LayoutReport>(
      module.checkLayout(text, JSON.stringify(images), name, flashSize(options)),
    ),
  );
}

/**
 * Writes the partition table, linker memory regions or Rust constants of the layout file `text`,
 * for `options.flashSize` when given (a layout with `size = "detect"`).
 */
export function writeLayout(
  text: string,
  format: LayoutFormat,
  name = "layout.toml",
  options: LayoutOptions = {},
): Promise<Uint8Array> {
  return run((module) => module.writeLayout(text, format, name, flashSize(options)));
}
