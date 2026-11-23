/** Who is to act: fix the input, the device, or the firmware that did not verify. */
export type ErrorClass = "input" | "device" | "verification";

const CLASSES: readonly string[] = ["input", "device", "verification"];

/** What went wrong, as the library tells it. */
export class NanoflashError extends Error {
  /** What kind of failure, such as `no-sync`, `verification-failed` or `not-granted`. */
  readonly kind: string;
  readonly errorClass: ErrorClass;
  /** The structured detail the library gave, without `kind`, `class` and `message`. */
  readonly details: Readonly<Record<string, unknown>>;
  /** What the library was doing, for errors of the port. */
  readonly during: string | undefined;
  /** What was thrown, as it was. */
  readonly raw: unknown;

  constructor(
    kind: string,
    errorClass: ErrorClass,
    message: string,
    details: Readonly<Record<string, unknown>> = {},
    options: { during?: string; raw?: unknown } = {},
  ) {
    super(message);
    this.name = "NanoflashError";
    this.kind = kind;
    this.errorClass = errorClass;
    this.details = details;
    this.during = options.during;
    this.raw = options.raw;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !(value instanceof Error);
}

/** Whether `error` is a trap of a WebAssembly module, which leaves the module unusable. */
export function isTrap(error: unknown): boolean {
  return typeof WebAssembly !== "undefined" && error instanceof WebAssembly.RuntimeError;
}

/**
 * Turns what a module threw into a `NanoflashError`. An error object of the library keeps its
 * `kind`, except a `transport` error, which takes the kind of the transport error inside it
 * (`not-granted`, `disconnected`); anything else thrown is an `internal` error of the device.
 */
export function fromWasm(value: unknown): NanoflashError {
  if (value instanceof NanoflashError) return value;
  if (isRecord(value) && typeof value["kind"] === "string") {
    const { kind, class: rawClass, message, ...details } = value;
    const errorClass = CLASSES.includes(rawClass as string) ? (rawClass as ErrorClass) : "device";
    const inner = details["error"];
    const innerKind = isRecord(inner) ? inner["kind"] : undefined;
    const outer = String(kind);
    const during = typeof details["during"] === "string" ? details["during"] : undefined;
    return new NanoflashError(
      outer === "transport" && typeof innerKind === "string" ? innerKind : outer,
      errorClass,
      typeof message === "string" ? message : outer,
      details,
      { raw: value, ...(during === undefined ? {} : { during }) },
    );
  }
  if (value instanceof Error) {
    return new NanoflashError("internal", "device", value.message, {}, { raw: value });
  }
  return new NanoflashError("internal", "device", String(value), {}, { raw: value });
}
