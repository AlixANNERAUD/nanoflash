// A Web Serial port that follows a transcript of crates/nanoflash/tests/transcripts, as the
// replay of the library does, with what differs in a browser: `! sleep` and `! timeout` lines are
// skipped (timing is real), the bytes of `<` lines are delivered as soon as every earlier event
// is consumed (consecutive `<` lines as one chunk), `! baud n` is a `close()` then an
// `open({ baudRate: n })`, and `! disconnect` loses the device. `! open-fails` is refused: a
// browser test gives `navigator.serial.getPorts()` no port instead.

/** Event kinds a browser session does not reproduce: they are skipped. */
const SKIPPED = new Set(["sleep", "timeout"]);

const ANY = "..";

/** The largest number of bytes a `..N` wildcard may take, as in the library. */
const MAXIMUM_COUNT = 16_777_216;

function parseByte(word, line) {
  if (!/^[0-9a-fA-F]{2}$/.test(word)) {
    throw new Error(`transcript line ${line}: \`${word}\` is not a hexadecimal byte`);
  }
  return Number.parseInt(word, 16);
}

/** `..N`: exactly `N` bytes of any value, as `{ count: N }`. */
function parseCount(word, line) {
  const digits = word.slice(2);
  const count = /^[0-9]+$/.test(digits) ? Number(digits) : Number.NaN;
  if (!Number.isSafeInteger(count) || count < 1 || count > MAXIMUM_COUNT) {
    throw new Error(
      `transcript line ${line}: \`${word}\` is not a count, write \`..\` and a number from 1 to ${MAXIMUM_COUNT}`,
    );
  }
  return { count };
}

const ESCAPES = { n: 0x0a, r: 0x0d, "\\": 0x5c, '"': 0x22 };

/**
 * Splits the text of a `>` or `<` line into words and quoted texts (`{ bytes }`): printable
 * ASCII, with the escapes `\n`, `\r`, `\\`, `\"` and `\xNN`.
 */
function pieces(text, line) {
  const found = [];
  const fail = (message) => {
    throw new Error(`transcript line ${line}: ${message}`);
  };
  let index = 0;
  while (index < text.length) {
    if (/\s/.test(text[index])) {
      index += 1;
    } else if (text[index] === '"') {
      const bytes = [];
      index += 1;
      for (;;) {
        if (index >= text.length) fail('a quoted text is not closed, add a closing `"`');
        const character = text[index];
        index += 1;
        if (character === '"') break;
        if (character === "\\") {
          const escape = text[index];
          index += 1;
          if (escape === "x") {
            const digits = text.slice(index, index + 2);
            if (!/^[0-9a-fA-F]{2}$/.test(digits)) {
              fail(`\`\\x${digits}\` is not an escape, write \`\\x\` and two hexadecimal digits`);
            }
            bytes.push(Number.parseInt(digits, 16));
            index += 2;
          } else if (escape !== undefined && Object.hasOwn(ESCAPES, escape)) {
            bytes.push(ESCAPES[escape]);
          } else if (escape === undefined) {
            fail('a quoted text is not closed, add a closing `"`');
          } else {
            fail(`\`\\${escape}\` is not an escape, use \`\\n\`, \`\\r\`, \`\\\\\`, \`\\"\` or \`\\xNN\``);
          }
        } else if (character >= " " && character <= "~") {
          bytes.push(character.charCodeAt(0));
        } else {
          fail(`\`${character}\` is not printable ASCII in a quoted text, use \`\\xNN\``);
        }
      }
      if (index < text.length && !/\s/.test(text[index])) {
        fail("a quoted text must be followed by a space or the end of the line");
      }
      found.push({ bytes });
    } else {
      const start = index;
      while (index < text.length && !/\s/.test(text[index])) index += 1;
      found.push(text.slice(start, index));
    }
  }
  return found;
}

/** The tokens of a `>` line: bytes, `..` and `{ count }`. */
function parseHost(text, line) {
  const tokens = [];
  for (const piece of pieces(text, line)) {
    if (typeof piece !== "string") tokens.push(...piece.bytes);
    else if (piece === ANY) tokens.push(ANY);
    else if (piece.startsWith("..")) tokens.push(parseCount(piece, line));
    else tokens.push(parseByte(piece, line));
  }
  if (tokens.length === 0) throw new Error(`transcript line ${line}: a \`>\` line needs at least one byte`);
  tokens.forEach((token, index) => {
    if (token === ANY && typeof tokens[index + 1] !== "number") {
      throw new Error(`transcript line ${line}: \`..\` must be followed by a literal byte on its line`);
    }
  });
  return tokens;
}

/** The bytes of a `<` line. */
function parseDevice(text, line) {
  const bytes = [];
  for (const piece of pieces(text, line)) {
    if (typeof piece !== "string") bytes.push(...piece.bytes);
    else if (piece.startsWith("..")) {
      throw new Error(`transcript line ${line}: \`..\` is only allowed in a \`>\` line`);
    } else bytes.push(parseByte(piece, line));
  }
  if (bytes.length === 0) throw new Error(`transcript line ${line}: a \`<\` line needs at least one byte`);
  return bytes;
}

function parseIdentity(words, line) {
  const match = /^([0-9a-fA-F]{1,4}):([0-9a-fA-F]{1,4})$/.exec(words[0] ?? "");
  if (match === null) {
    throw new Error(`transcript line ${line}: \`${words[0]}\` is not an identity like 303a:1001`);
  }
  const serialNumber = words.length > 1 ? words.slice(1).join(" ") : null;
  return {
    vendor: Number.parseInt(match[1], 16),
    product: Number.parseInt(match[2], 16),
    serialNumber,
  };
}

function parseFlag(word, name, line) {
  if (word === `${name}=1`) return true;
  if (word === `${name}=0`) return false;
  throw new Error(`transcript line ${line}: expected \`${name}=0\` or \`${name}=1\`, got \`${word}\``);
}

/** Reads a transcript into its identity and its events, each with its line and text. */
export function parseTranscript(text) {
  let identity = null;
  const events = [];
  text.split(/\r?\n/).forEach((raw, index) => {
    const line = index + 1;
    const trimmed = raw.trim();
    if (trimmed === "" || trimmed.startsWith("#")) return;
    const rest = trimmed.slice(1).trim();
    const words = rest.split(/\s+/).filter((word) => word !== "");
    const event = { line, text: trimmed };
    if (trimmed.startsWith(">")) {
      event.kind = "host";
      event.tokens = parseHost(rest, line);
    } else if (trimmed.startsWith("<")) {
      event.kind = "device";
      event.bytes = parseDevice(rest, line);
    } else if (trimmed.startsWith("!")) {
      const [name, ...argumentsList] = words;
      switch (name) {
        case "identity":
          if (events.length > 0 || identity !== null) {
            throw new Error(`transcript line ${line}: \`identity\` must be the first event`);
          }
          identity = parseIdentity(argumentsList, line);
          return;
        case "signals": {
          const dtr = argumentsList.find((word) => word.startsWith("dtr"));
          const rts = argumentsList.find((word) => word.startsWith("rts"));
          event.kind = "signals";
          event.dtr = parseFlag(dtr, "dtr", line);
          event.rts = parseFlag(rts, "rts", line);
          break;
        }
        case "baud":
          event.kind = "baud";
          event.baudRate = Number.parseInt(argumentsList[0], 10);
          break;
        case "sleep":
        case "timeout":
        case "disconnect":
          event.kind = name;
          break;
        case "reopen":
          event.kind = "reopen";
          event.identity = argumentsList.length > 0 ? parseIdentity(argumentsList, line) : null;
          break;
        case "open-fails":
          if (argumentsList.length !== 1 || !["not-found", "not-granted"].includes(argumentsList[0])) {
            throw new Error(
              `transcript line ${line}: expected \`open-fails not-found\` or \`open-fails not-granted\``,
            );
          }
          event.kind = "open-fails";
          event.failure = argumentsList[0];
          break;
        default:
          throw new Error(`transcript line ${line}: unknown directive \`${name}\``);
      }
    } else {
      throw new Error(`transcript line ${line}: a line starts with \`>\`, \`<\`, \`!\` or \`#\``);
    }
    events.push(event);
  });
  return { identity, events };
}

function hex(byte) {
  return byte.toString(16).padStart(2, "0");
}

function lostDevice() {
  return new DOMException("The device has been lost.", "NetworkError");
}

/**
 * A port following `transcriptText`. `finish()` throws naming the first event left, sleeps and
 * timeouts aside. The port records its calls in `log` (`open 115200`, `close`) and the size of
 * every chunk it delivered in `chunks`. With `stallWrites`, no write ever completes, as with a
 * device that stopped reading.
 */
export function mockPort(transcriptText, { stallWrites = false } = {}) {
  const { identity, events } = parseTranscript(transcriptText);
  const failing = events.find((event) => event.kind === "open-fails");
  if (failing !== undefined) {
    // A failed open happens in the provider, which lists the ports of `navigator.serial`.
    throw new Error(
      `transcript line ${failing.line}: \`${failing.text}\` is not reproducible in a browser: give navigator.serial.getPorts() no port instead`,
    );
  }
  let next = 0;
  // Within a `>` event: the token the next byte is compared with, and the bytes a `..` took.
  let hostPosition = 0;
  let wildcardCount = 0;
  let lost = false;
  let open = false;
  let expectedBaudRate = null;
  let readable = null;
  let writable = null;
  let readableController = null;
  let writableController = null;
  // Chunks reached while the port is closed, delivered when it opens.
  const queued = [];
  const log = [];
  const chunks = [];

  const current = () => events[next];

  const divergence = (got) => {
    const event = current();
    if (event === undefined) {
      return new Error(`transcript: expected the end of the transcript, got ${got}`);
    }
    return new Error(`transcript line ${event.line}: expected ${event.text}, got ${got}`);
  };

  const deliver = (chunk) => {
    chunks.push(chunk.length);
    if (readableController === null) {
      queued.push(chunk);
    } else {
      readableController.enqueue(chunk);
    }
  };

  const loseDevice = () => {
    lost = true;
    try {
      readableController?.error(lostDevice());
    } catch {
      // Already closed or errored.
    }
    try {
      writableController?.error(lostDevice());
    } catch {
      // Already closed or errored.
    }
  };

  // Moves past the current event, then delivers what the device sends next.
  const advance = () => {
    next += 1;
    hostPosition = 0;
    wildcardCount = 0;
    settle();
  };

  function settle() {
    for (;;) {
      const event = current();
      if (event === undefined) return;
      if (SKIPPED.has(event.kind)) {
        next += 1;
      } else if (event.kind === "device") {
        const bytes = [];
        while (current()?.kind === "device") {
          bytes.push(...current().bytes);
          next += 1;
        }
        deliver(new Uint8Array(bytes));
      } else if (event.kind === "disconnect") {
        next += 1;
        loseDevice();
      } else {
        return;
      }
    }
  }

  // Compares one written byte with the current `>` event, as the replay of the library does.
  const writeByte = (byte) => {
    const event = current();
    if (event?.kind !== "host") throw divergence("a write");
    const { tokens } = event;
    const token = tokens[hostPosition];
    if (typeof token === "object") {
      // `..N`: the Nth byte ends it.
      if (wildcardCount + 1 >= token.count) {
        hostPosition += 1;
        wildcardCount = 0;
      } else {
        wildcardCount += 1;
      }
    } else if (token === ANY) {
      if (wildcardCount > 0 && tokens[hostPosition + 1] === byte) {
        hostPosition += 2;
        wildcardCount = 0;
      } else {
        wildcardCount += 1;
      }
    } else if (token === byte) {
      hostPosition += 1;
      wildcardCount = 0;
    } else {
      throw new Error(
        `transcript line ${event.line}: expected ${hex(token)}, got ${hex(byte)} in ${event.text}`,
      );
    }
    if (hostPosition >= tokens.length) advance();
  };

  const createStreams = () => {
    readable = new ReadableStream({
      start(controller) {
        readableController = controller;
      },
      cancel() {
        readableController = null;
      },
    });
    writable = new WritableStream({
      start(controller) {
        writableController = controller;
      },
      write(chunk) {
        if (stallWrites) return new Promise(() => {});
        if (lost) throw lostDevice();
        if (!(chunk instanceof Uint8Array)) {
          throw new TypeError("a Web Serial port only takes Uint8Array chunks");
        }
        for (const byte of chunk) writeByte(byte);
      },
    });
    for (const chunk of queued.splice(0)) readableController.enqueue(chunk);
  };

  const port = {
    get isOpen() {
      return open;
    },
    get readable() {
      return readable;
    },
    get writable() {
      return writable;
    },
    log,
    chunks,
    async open(options) {
      const baudRate = options?.baudRate;
      if (open) {
        throw new DOMException("The port is already open.", "InvalidStateError");
      }
      if (lost) throw lostDevice();
      if (!Number.isInteger(baudRate) || baudRate <= 0) {
        throw new TypeError(`invalid baudRate ${baudRate}`);
      }
      if (expectedBaudRate !== null) {
        if (baudRate !== expectedBaudRate) throw divergence(`an open at ${baudRate} baud`);
        expectedBaudRate = null;
        log.push(`open ${baudRate}`);
        open = true;
        createStreams();
        advance();
        return;
      }
      log.push(`open ${baudRate}`);
      open = true;
      createStreams();
    },
    async close() {
      if (!open) {
        throw new DOMException("The port is already closed.", "InvalidStateError");
      }
      if (readable?.locked || writable?.locked) {
        throw new TypeError("Failed to execute 'close' on 'SerialPort': Cannot cancel a locked stream");
      }
      log.push("close");
      open = false;
      try {
        readableController?.close();
      } catch {
        // Already cancelled or errored.
      }
      readable = null;
      writable = null;
      readableController = null;
      writableController = null;
      if (current()?.kind === "baud") expectedBaudRate = current().baudRate;
    },
    async setSignals(signals) {
      if (lost) throw lostDevice();
      if (!open) throw new DOMException("The port is closed.", "InvalidStateError");
      const dtr = signals?.dataTerminalReady === true;
      const rts = signals?.requestToSend === true;
      const event = current();
      if (event?.kind !== "signals" || event.dtr !== dtr || event.rts !== rts) {
        throw divergence(`! signals dtr=${Number(dtr)} rts=${Number(rts)}`);
      }
      advance();
    },
    getInfo() {
      if (identity === null) return {};
      return { usbVendorId: identity.vendor, usbProductId: identity.product };
    },
  };

  settle();

  const finish = () => {
    const left = events.slice(next).find((event) => !SKIPPED.has(event.kind));
    if (left !== undefined) {
      throw new Error(`the session ended before line ${left.line}: ${left.text}`);
    }
  };

  return { port, finish };
}
