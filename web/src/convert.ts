/** Whether `value` is a plain object, as JSON and the WebAssembly modules give them. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null) return false;
  const prototype: unknown = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function camelCase(key: string): string {
  return key.replace(/[_-]+([a-zA-Z0-9])/g, (_, letter: string) => letter.toUpperCase());
}

/** Converts, recursively, the keys of objects from `snake_case` or `kebab-case` to `camelCase`. Values are untouched. */
export function camelize<T>(value: unknown): T {
  if (Array.isArray(value)) return value.map((item) => camelize<unknown>(item)) as T;
  if (isRecord(value)) {
    const result: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) result[camelCase(key)] = camelize(item);
    return result as T;
  }
  return value as T;
}

/** Whether the browser has Web Serial (Chromium based browsers only). */
export function isSupported(): boolean {
  return typeof navigator !== "undefined" && "serial" in navigator;
}
