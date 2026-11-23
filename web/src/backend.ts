import { NanoflashError, fromWasm, isTrap } from "./errors.js";
import type { Protocol, SerialPortLike } from "./types.js";

/** The WebAssembly module of a protocol. */
export interface BackendModule {
  flash(
    manifest: string,
    port: SerialPortLike,
    options: string,
    readFile: (path: string) => Promise<Uint8Array>,
    progress: (event: unknown) => void,
  ): Promise<unknown>;
  detect(
    port: SerialPortLike,
    options: string,
    progress: (event: unknown) => void,
  ): Promise<unknown>;
}

interface WasmModule extends BackendModule {
  default(): Promise<unknown>;
}

// Static imports, so that bundlers split each module into a chunk of its own.
const modules: Partial<Record<Protocol, () => Promise<WasmModule>>> = {
  espressif: () => import("./wasm/nanoflash_espressif.js"),
};

const cache = new Map<Protocol, Promise<BackendModule>>();

/** Loads and initialises the module of `protocol`, once. */
export function loadBackend(protocol: Protocol): Promise<BackendModule> {
  const cached = cache.get(protocol);
  if (cached !== undefined) return cached;
  const load = modules[protocol];
  if (load === undefined) {
    return Promise.reject(
      new NanoflashError(
        "protocol-not-enabled",
        "input",
        `the ${protocol} protocol has no module in this build of nanoflash`,
      ),
    );
  }
  const promise: Promise<BackendModule> = load()
    .then(async (module) => {
      await module.default();
      return { flash: module.flash, detect: module.detect };
    })
    .catch((error: unknown) => {
      // Not cached: the next call tries again.
      if (cache.get(protocol) === promise) cache.delete(protocol);
      throw fromWasm(error);
    });
  cache.set(protocol, promise);
  return promise;
}

/** Forgets the module of `protocol`, so that the next call instantiates it again. */
export function dropBackend(protocol: Protocol): void {
  cache.delete(protocol);
}

const CLOSE_TIMEOUT_MS = 3000;

/** Closes `port`, ignoring errors and not waiting more than a few seconds. */
export async function closePort(port: SerialPortLike): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      port.close(),
      new Promise<void>((resolve) => {
        timer = setTimeout(resolve, CLOSE_TIMEOUT_MS);
      }),
    ]);
  } catch {
    // Already closed, or locked by a stream that no longer matters.
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/**
 * Turns what a call of a module threw into a `NanoflashError`. A trap leaves the module in an
 * unknown state: `drop` forgets it and the ports the call held are closed.
 */
export async function settle(
  error: unknown,
  drop: () => void,
  ports: readonly SerialPortLike[] = [],
): Promise<NanoflashError> {
  if (isTrap(error)) {
    drop();
    await Promise.all(ports.map(closePort));
  }
  return fromWasm(error);
}
