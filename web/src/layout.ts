import { camelize } from "./convert.js";
import { assertUsable, settle } from "./backend.js";
import type { LayoutFormat, LayoutReport } from "./types.js";

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

/**
 * Checks the layout file `text` (TOML), with the sizes of the images that go in its partitions,
 * and tells the space used and left in each. `name` is the file name, for messages.
 */
export function checkLayout(
  text: string,
  images: { partition: string; size: number }[] = [],
  name = "layout.toml",
): Promise<LayoutReport> {
  return run((module) => camelize<LayoutReport>(module.checkLayout(text, JSON.stringify(images), name)));
}

/** Writes the partition table, linker memory regions or Rust constants of the layout file `text`. */
export function writeLayout(
  text: string,
  format: LayoutFormat,
  name = "layout.toml",
): Promise<Uint8Array> {
  return run((module) => module.writeLayout(text, format, name));
}
