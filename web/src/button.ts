import { NanoflashError } from "./errors.js";
import { prepare } from "./flash.js";
import type { Prepared } from "./flash.js";
import { isSupported } from "./convert.js";
import type { FlashReport, ProgressEvent, SerialPortLike } from "./types.js";

const UNSUPPORTED =
  "This browser cannot reach serial ports. Use Chrome or Edge on a computer, or the nanoflash command line.";

const STYLE = `
:host { display: inline-block; font-family: var(--nanoflash-font, inherit); }
[hidden] { display: none !important; }
button {
  font: inherit; cursor: pointer; border: 0; padding: 0.6em 1.2em;
  border-radius: var(--nanoflash-button-radius, 6px);
  background: var(--nanoflash-button-background, #1f6feb);
  color: var(--nanoflash-button-color, #ffffff);
}
button:disabled { opacity: 0.5; cursor: not-allowed; }
label { display: block; margin: 0.5em 0; }
progress { display: block; width: 100%; accent-color: var(--nanoflash-progress-color, #1f6feb); }
p { margin: 0.5em 0 0; }
`;

const WORKING = ["connecting", "erasing", "writing", "verifying", "resetting"];

function chooserClosed(error: unknown): boolean {
  const raw = error instanceof NanoflashError ? error.raw : error;
  return typeof raw === "object" && raw !== null && (raw as { name?: unknown }).name === "NotFoundError";
}

/** An install button for a manifest: `<nanoflash-button manifest="firmware/manifest.json">`. */
export class NanoflashButton extends HTMLElement {
  static observedAttributes = ["manifest", "label", "erase-all"];

  readonly #button: HTMLButtonElement;
  readonly #erase: HTMLLabelElement;
  readonly #checkbox: HTMLInputElement;
  readonly #progress: HTMLProgressElement;
  readonly #message: HTMLParagraphElement;
  readonly #bootloader: HTMLButtonElement;
  #prepared: Prepared | undefined;
  /** Counts manifest loads, so a late answer for an old manifest is dropped. */
  #generation = 0;
  /** Set once connected, so the attributes present at upgrade do not load the manifest twice. */
  #started = false;

  constructor() {
    super();
    const root = this.attachShadow({ mode: "open" });
    const style = document.createElement("style");
    style.textContent = STYLE;
    this.#button = document.createElement("button");
    this.#button.setAttribute("part", "button");
    this.#erase = document.createElement("label");
    this.#erase.setAttribute("part", "erase");
    this.#checkbox = document.createElement("input");
    this.#checkbox.type = "checkbox";
    this.#erase.append(this.#checkbox, " Erase the whole flash first");
    this.#progress = document.createElement("progress");
    this.#progress.setAttribute("part", "progress");
    this.#progress.max = 100;
    this.#progress.value = 0;
    this.#message = document.createElement("p");
    this.#message.setAttribute("part", "message");
    this.#message.setAttribute("role", "status");
    this.#message.setAttribute("aria-live", "polite");
    this.#bootloader = document.createElement("button");
    this.#bootloader.setAttribute("part", "bootloader-button");
    this.#bootloader.textContent = "Select the board in bootloader mode";
    root.append(style, this.#button, this.#erase, this.#progress, this.#message, this.#bootloader);
    this.#button.addEventListener("click", () => this.#install());
    this.#bootloader.addEventListener("click", () => this.#installWithBootloader());
    this.#refresh();
  }

  connectedCallback(): void {
    this.#started = true;
    void this.#load();
  }

  attributeChangedCallback(name: string): void {
    if (name === "manifest") {
      if (this.#started && this.isConnected) void this.#load();
    } else {
      this.#refresh();
    }
  }

  #render(state: string, message: string): void {
    this.dataset["state"] = state;
    this.#message.textContent = message;
    this.#refresh();
  }

  #refresh(): void {
    const state = this.dataset["state"] ?? "";
    const working = WORKING.includes(state);
    this.#button.textContent = this.getAttribute("label") ?? "Install";
    this.#button.disabled =
      this.#prepared === undefined || !(state === "ready" || state === "done" || state === "error");
    this.#erase.hidden = !(this.#prepared?.promptErase === true) || this.hasAttribute("erase-all");
    this.#checkbox.disabled = working;
    this.#progress.hidden = !working;
    this.#bootloader.hidden = state !== "bootloader-permission";
  }

  async #load(): Promise<void> {
    const generation = ++this.#generation;
    this.#prepared = undefined;
    if (!isSupported()) {
      this.#render("unsupported", UNSUPPORTED);
      return;
    }
    const manifest = this.getAttribute("manifest");
    if (manifest === null) {
      this.#render("error", "The manifest attribute is missing.");
      return;
    }
    this.#render("loading", "");
    try {
      const prepared = await prepare(manifest);
      if (generation !== this.#generation) return;
      this.#prepared = prepared;
      this.#render("ready", "");
    } catch (error) {
      if (generation !== this.#generation) return;
      this.#fail(error);
    }
  }

  #options(port?: SerialPortLike): Parameters<Prepared["flash"]>[0] {
    return {
      ...(port === undefined ? {} : { port }),
      eraseAll: this.hasAttribute("erase-all") || this.#checkbox.checked,
      onProgress: (event: ProgressEvent) => {
        this.#progress.value = event.total === 0 ? 0 : (event.done * 100) / event.total;
        this.#render(event.stage, "");
        this.#emit("nanoflash-progress", event);
      },
    };
  }

  /** Runs in the click: `flash` calls `requestPort` before its first await. */
  #install(): void {
    const prepared = this.#prepared;
    if (prepared === undefined) return;
    this.#run(prepared.flash(this.#options()));
  }

  #installWithBootloader(): void {
    const prepared = this.#prepared;
    const serial = (navigator as { serial?: { requestPort(options: object): Promise<unknown> } }).serial;
    if (prepared === undefined || serial === undefined) return;
    this.#bootloader.disabled = true;
    const filters = prepared.bootloaderFilters;
    const chosen = serial.requestPort(filters.length === 0 ? {} : { filters });
    void chosen.then(
      (port) => {
        this.#bootloader.disabled = false;
        this.#run(prepared.flash(this.#options(port as SerialPortLike)));
      },
      (error: unknown) => {
        this.#bootloader.disabled = false;
        if (!chooserClosed(error)) this.#fail(error);
      },
    );
  }

  #run(flashing: Promise<FlashReport>): void {
    this.#progress.value = 0;
    this.#render("connecting", "");
    flashing.then(
      (report) => {
        this.#render("done", `Done: ${report.board} was written and restarted.`);
        this.#emit("nanoflash-done", report);
      },
      (error: unknown) => {
        if (chooserClosed(error)) {
          this.#render("ready", "");
        } else if (error instanceof NanoflashError && error.kind === "not-granted") {
          this.#render("bootloader-permission", error.message);
        } else {
          this.#fail(error);
        }
      },
    );
  }

  #fail(error: unknown): void {
    const message = error instanceof Error ? error.message : String(error);
    this.#render("error", message);
    this.#emit("nanoflash-error", error);
  }

  #emit(name: string, detail: unknown): void {
    this.dispatchEvent(new CustomEvent(name, { detail, bubbles: true, composed: true }));
  }
}

if (typeof customElements !== "undefined" && customElements.get("nanoflash-button") === undefined) {
  customElements.define("nanoflash-button", NanoflashButton);
}
