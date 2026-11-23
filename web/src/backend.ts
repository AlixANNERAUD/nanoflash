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

// Modules that trapped. An ES module keeps its WebAssembly instance, which cannot be replaced
// after a trap: every later call fails at once, and the page has to be reloaded.
const broken = new Map<string, NanoflashError>();

/** Throws the error of the module `key` (a protocol, or `core`) if it trapped before. */
export function assertUsable(key: string): void {
  const error = broken.get(key);
  if (error !== undefined) throw error;
}

/** Loads and initialises the module of `protocol`, once. */
export function loadBackend(protocol: Protocol): Promise<BackendModule> {
  const failure = broken.get(protocol);
  if (failure !== undefined) return Promise.reject(failure);
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
 * Turns what a call of the module `key` threw into a `NanoflashError`. A trap leaves the module
 * in an unknown state and it cannot be instantiated again: the ports the call held are closed,
 * the module is marked broken, and every later call on it fails with `module-broken` (the page
 * has to be reloaded).
 */
export async function settle(
  error: unknown,
  key: string,
  ports: readonly SerialPortLike[] = [],
): Promise<NanoflashError> {
  if (!isTrap(error)) return fromWasm(error);
  await Promise.all(ports.map(closePort));
  const existing = broken.get(key);
  if (existing !== undefined) return existing;
  const reason = error instanceof Error ? error.message : String(error);
  const failure = new NanoflashError(
    "module-broken",
    "device",
    `the WebAssembly module "${key}" stopped on an unexpected failure (${reason}) and cannot be used again: reload the page`,
    {},
    { raw: error },
  );
  broken.set(key, failure);
  return failure;
}

/**
 * Calls `listener` with every uncaught WebAssembly trap until the returned function is called:
 * the `error` events of the page, and in Node `uncaughtExceptionMonitor`, which leaves Node's
 * own handling of the exception as it is.
 */
function watchTraps(listener: (error: unknown) => void): () => void {
  type Target = {
    addEventListener?(type: string, listener: (event: Event) => void): void;
    removeEventListener?(type: string, listener: (event: Event) => void): void;
  };
  type Process = {
    on?(event: string, listener: (error: unknown) => void): void;
    off?(event: string, listener: (error: unknown) => void): void;
  };
  const page = globalThis as Target;
  const node = (globalThis as { process?: Process }).process;
  const onError = (event: Event) => {
    const error: unknown = (event as { error?: unknown }).error;
    if (isTrap(error)) listener(error);
  };
  const onException = (error: unknown) => {
    if (isTrap(error)) listener(error);
  };
  page.addEventListener?.("error", onError);
  node?.on?.("uncaughtExceptionMonitor", onException);
  return () => {
    page.removeEventListener?.("error", onError);
    node?.off?.("uncaughtExceptionMonitor", onException);
  };
}

/**
 * Runs `call`, an asynchronous call of the module `key` that uses `ports`, and turns what it
 * throws into a `NanoflashError` as `settle` does. wasm-bindgen runs asynchronous exports in
 * microtasks, where a trap is an uncaught exception and the promise of the call never settles:
 * while the call runs, an uncaught trap (of any module) rejects it as well.
 */
export async function guard<T>(
  key: string,
  ports: readonly SerialPortLike[],
  call: () => Promise<T>,
): Promise<T> {
  let stop = () => {};
  const trapped = new Promise<never>((_, reject) => {
    stop = watchTraps(reject);
  });
  try {
    return await Promise.race([call(), trapped]);
  } catch (error) {
    throw await settle(error, key, ports);
  } finally {
    stop();
  }
}
