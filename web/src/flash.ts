import { assertUsable, guard, loadBackend } from "./backend.js";
import { camelize } from "./convert.js";
import { NanoflashError } from "./errors.js";
import type {
  ChipInformation,
  FlashOptions,
  FlashReport,
  Protocol,
  ProgressEvent,
  SerialPortFilter,
  SerialPortLike,
} from "./types.js";

const PROTOCOLS: readonly string[] = ["espressif", "samba"];

/** A manifest read, and what a browser needs to flash it. */
export interface Prepared {
  readonly manifestUrl: URL;
  readonly protocols: readonly Protocol[];
  /** Filters for `requestPort`: the devices of the boards; empty when any board lists none. */
  readonly filters: readonly SerialPortFilter[];
  /** The devices of the boards when they are in their bootloader. */
  readonly bootloaderFilters: readonly SerialPortFilter[];
  /** Whether to offer to erase the whole flash. */
  readonly promptErase: boolean;
  flash(options?: FlashOptions): Promise<FlashReport>;
}

type Json = Record<string, unknown>;

function isJson(value: unknown): value is Json {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function manifestError(message: string): NanoflashError {
  return new NanoflashError("manifest", "input", message);
}

/** A USB identifier as `quantity::parse` reads it (decimal, `0x` hexadecimal, a unit), at most 0xffff. */
function parseIdentifier(value: unknown, what: string): number {
  const invalid = () =>
    manifestError(
      `${what} must be a whole number from 0 to 65535, in decimal or as 0x followed by hexadecimal digits, found ${JSON.stringify(value)}`,
    );
  let number: number;
  if (typeof value === "number") {
    number = value;
  } else if (typeof value === "string") {
    const text = value.replace(/^ +| +$/g, "");
    if (/^0[xX][0-9a-fA-F]+$/.test(text)) {
      number = Number.parseInt(text.slice(2), 16);
    } else {
      const match = /^([0-9]+)( *(B|KiB|MiB|GiB))?$/.exec(text);
      if (match === null) throw invalid();
      const units: Record<string, number> = { B: 1, KiB: 1 << 10, MiB: 1 << 20, GiB: 2 ** 30 };
      number = Number(match[1]) * (match[3] === undefined ? 1 : (units[match[3]] ?? 1));
    }
  } else {
    throw invalid();
  }
  if (!Number.isInteger(number) || number < 0 || number > 0xffff) throw invalid();
  return number;
}

function parseDevices(board: string, field: string, value: unknown): SerialPortFilter[] {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw manifestError(`"${field}" of the board "${board}" must be a list`);
  return value.map((device: unknown) => {
    if (!isJson(device) || device["vendor"] === undefined) {
      throw manifestError(`every entry of "${field}" of the board "${board}" needs a "vendor"`);
    }
    const where = `the "${field}" of the board "${board}"`;
    const filter: SerialPortFilter = {
      usbVendorId: parseIdentifier(device["vendor"], `the "vendor" of ${where}`),
    };
    if (device["product"] !== undefined) {
      filter.usbProductId = parseIdentifier(device["product"], `the "product" of ${where}`);
    }
    return filter;
  });
}

function dedupe(filters: SerialPortFilter[]): SerialPortFilter[] {
  const seen = new Set<string>();
  return filters.filter((filter) => {
    const key = `${filter.usbVendorId}:${filter.usbProductId ?? ""}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

interface Reading {
  protocols: Protocol[];
  filters: SerialPortFilter[];
  bootloaderFilters: SerialPortFilter[];
  promptErase: boolean;
  /** The protocol of each board, by identifier. */
  boards: Map<string, Protocol>;
}

function read(value: unknown): Reading {
  if (isJson(value) && Array.isArray(value["builds"])) {
    return {
      protocols: ["espressif"],
      filters: [],
      bootloaderFilters: [],
      promptErase: value["new_install_prompt_erase"] === true,
      boards: new Map(),
    };
  }
  if (!isJson(value) || !Array.isArray(value["board"]) || value["board"].length === 0) {
    throw manifestError(
      'the manifest lists no board: add a "board" array (or "builds", for an esp-web-tools manifest)',
    );
  }
  const protocols: Protocol[] = [];
  const boards = new Map<string, Protocol>();
  const application: SerialPortFilter[] = [];
  const bootloader: SerialPortFilter[] = [];
  let everyBoardHasDevices = true;
  for (const board of value["board"] as unknown[]) {
    if (!isJson(board)) throw manifestError('every "board" of the manifest must be a table');
    const identifier = board["identifier"];
    const protocol = board["protocol"];
    if (typeof identifier !== "string") throw manifestError('every board needs an "identifier"');
    if (typeof protocol !== "string" || !PROTOCOLS.includes(protocol)) {
      throw manifestError(
        `the protocol of the board "${identifier}" must be "espressif" or "samba", found ${JSON.stringify(protocol)}`,
      );
    }
    if (!protocols.includes(protocol as Protocol)) protocols.push(protocol as Protocol);
    boards.set(identifier, protocol as Protocol);
    const own = parseDevices(identifier, "application-devices", board["application-devices"]);
    const boot = parseDevices(identifier, "bootloader-devices", board["bootloader-devices"]);
    if (own.length + boot.length === 0) everyBoardHasDevices = false;
    application.push(...own);
    bootloader.push(...boot);
  }
  return {
    protocols,
    filters: everyBoardHasDevices ? dedupe([...application, ...bootloader]) : [],
    bootloaderFilters: dedupe(bootloader),
    promptErase: value["prompt-erase"] === true,
    boards,
  };
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Asks the user for a port. Call it before any other `await` of a click handler. A closed
 * chooser rejects with `no-port-chosen`; a chooser the browser refused to show, with
 * `port-request-refused`.
 */
export function requestPort(filters: readonly SerialPortFilter[]): Promise<SerialPortLike> {
  const serial = (globalThis.navigator as { serial?: { requestPort(options: object): Promise<unknown> } } | undefined)
    ?.serial;
  if (serial === undefined) {
    return Promise.reject(
      new NanoflashError(
        "unsupported-browser",
        "input",
        "this browser has no Web Serial: use a Chromium based browser, or the nanoflash command line",
      ),
    );
  }
  return serial
    .requestPort(filters.length === 0 ? {} : { filters })
    .then((port) => port as SerialPortLike)
    .catch((error: unknown) => {
      if ((error as { name?: unknown } | null)?.name === "NotFoundError") {
        throw new NanoflashError("no-port-chosen", "input", "no port was chosen", {}, { raw: error });
      }
      throw new NanoflashError(
        "port-request-refused",
        "input",
        `the browser did not show the port chooser (${describe(error)}): a page in an iframe needs allow="serial", and a permissions policy may block Web Serial`,
        {},
        { raw: error },
      );
    });
}

function progressHandler(options: {
  onProgress?: ((event: ProgressEvent) => void) | undefined;
  onMessage?: ((text: string) => void) | undefined;
}): (event: unknown) => void {
  return (event) => {
    if (!isJson(event)) return;
    if (typeof event["stage"] === "string") options.onProgress?.(event as unknown as ProgressEvent);
    else if (typeof event["message"] === "string") options.onMessage?.(event["message"]);
  };
}

/** Reads the manifest at `manifestUrl`, which is JSON, ours or esp-web-tools. */
export async function prepare(manifestUrl: string | URL): Promise<Prepared> {
  const base = typeof document !== "undefined" ? document.baseURI : undefined;
  let url: URL;
  try {
    url = new URL(manifestUrl.toString(), base);
  } catch {
    throw new NanoflashError("image-source", "input", `the manifest URL "${manifestUrl.toString()}" is not a valid absolute URL`);
  }
  let text: string;
  try {
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`the server answered ${response.status} ${response.statusText}`.trim());
    }
    text = await response.text();
  } catch (error) {
    throw new NanoflashError("image-source", "input", `could not read the manifest ${url.href}: ${describe(error)}`, {}, { raw: error });
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw manifestError(`the browser reads JSON manifests only; convert ${url.href} to JSON (same keys)`);
  }
  const reading = read(parsed);
  if (reading.protocols.length === 1 && reading.protocols[0] !== undefined) {
    // Warm up the module while the user decides; a failure shows when it is needed.
    loadBackend(reading.protocols[0]).catch(() => undefined);
  }

  const chooseProtocol = (boardIdentifier: string | undefined): Protocol => {
    const chosen = boardIdentifier === undefined ? undefined : reading.boards.get(boardIdentifier);
    if (chosen !== undefined) return chosen;
    const [only, ...others] = reading.protocols;
    if (only !== undefined && others.length === 0) return only;
    throw new NanoflashError(
      "several-protocols",
      "input",
      `the manifest has boards for several protocols (${reading.protocols.join(", ")}): give the boardIdentifier of the board to flash`,
    );
  };

  return {
    manifestUrl: url,
    protocols: reading.protocols,
    filters: reading.filters,
    bootloaderFilters: reading.bootloaderFilters,
    promptErase: reading.promptErase,
    async flash(options: FlashOptions = {}): Promise<FlashReport> {
      const protocol = chooseProtocol(options.boardIdentifier);
      assertUsable(protocol);
      // The first await: a click handler loses its user activation after others.
      const port = options.port ?? (await requestPort(reading.filters));
      const backend = await loadBackend(protocol);
      const readFile = async (path: string): Promise<Uint8Array> => {
        const file = new URL(path, url);
        let response: Response;
        try {
          response = await fetch(file);
        } catch (error) {
          throw new Error(`could not fetch ${file.href}: ${describe(error)}`);
        }
        if (!response.ok) {
          throw new Error(`could not fetch ${file.href}: the server answered ${response.status} ${response.statusText}`.trim());
        }
        return new Uint8Array(await response.arrayBuffer());
      };
      const report = await guard(protocol, [port], () =>
        backend.flash(
          text,
          port,
          JSON.stringify({
            board_identifier: options.boardIdentifier ?? null,
            erase_all: options.eraseAll ?? false,
            verify: options.verify ?? true,
            baud_rate: options.baudRate ?? null,
          }),
          readFile,
          progressHandler(options),
        ),
      );
      return camelize<FlashReport>(report);
    },
  };
}

/** Reads the manifest at `manifestUrl` and flashes the board it describes. */
export async function flash(manifestUrl: string | URL, options: FlashOptions = {}): Promise<FlashReport> {
  return (await prepare(manifestUrl)).flash(options);
}

/** Tells what chip the device on a port is. */
export async function detect(options: {
  protocol: Protocol;
  port?: SerialPortLike;
  onProgress?: (event: ProgressEvent) => void;
}): Promise<ChipInformation> {
  assertUsable(options.protocol);
  const port = options.port ?? (await requestPort([]));
  const backend = await loadBackend(options.protocol);
  const chip = await guard(options.protocol, [port], () =>
    backend.detect(port, JSON.stringify({ protocol: options.protocol }), progressHandler(options)),
  );
  return camelize<ChipInformation>(chip);
}
