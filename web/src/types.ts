/** A flashing protocol. `samba` is known, but no module speaks it yet. */
export type Protocol = "espressif" | "samba";

/** What the package needs from a Web Serial `SerialPort` (a mock fits as well). */
export interface SerialPortLike {
  open(options: { baudRate: number; bufferSize?: number }): Promise<void>;
  close(): Promise<void>;
  readonly readable: ReadableStream<Uint8Array> | null;
  readonly writable: WritableStream<Uint8Array> | null;
  setSignals(signals: { dataTerminalReady?: boolean; requestToSend?: boolean }): Promise<void>;
  getInfo(): { usbVendorId?: number; usbProductId?: number };
}

/** A filter of `navigator.serial.requestPort`. */
export interface SerialPortFilter {
  usbVendorId: number;
  usbProductId?: number;
}

/** A step of a flash. `done` and `total` are in bytes. */
export interface ProgressEvent {
  stage: "connecting" | "erasing" | "writing" | "verifying" | "resetting";
  offset: number;
  done: number;
  total: number;
}

/** What `flash` does and where it reports. */
export interface FlashOptions {
  /** The port to use; else the user is asked to choose one. */
  port?: SerialPortLike;
  /** The board of the manifest to flash; else the one the chip is. */
  boardIdentifier?: string;
  /** Erase the whole flash first. */
  eraseAll?: boolean;
  /** Read back and check what was written (default `true`). */
  verify?: boolean;
  /** The baud rate to flash at. */
  baudRate?: number;
  onProgress?: (event: ProgressEvent) => void;
  onMessage?: (text: string) => void;
}

/** What the device on a port is. */
export interface ChipInformation {
  protocol: Protocol;
  chip: string;
  flashSize: number | null;
  secureDownloadMode: boolean;
  usb: { vendor: number; product: number; serialNumber: string | null } | null;
}

/** Something to know, which did not stop the flash. `message` is what to show. */
export type FlashWarning =
  | { kind: "partition-table-differs"; partition: string; message: string }
  | { kind: "partition-table-not-checked"; partition: string; message: string }
  | { kind: "not-verified-in-secure-download-mode"; message: string };

/** What `flash` did. */
export interface FlashReport {
  board: string;
  chip: ChipInformation;
  images: { label: string; offset: number; size: number }[];
  verified: boolean;
  warnings: FlashWarning[];
}

/** The space used and left in one partition. */
export interface PartitionReport {
  name: string;
  role: string;
  offset: number;
  end: number;
  size: number;
  imageSize: number | null;
  headroom: number | null;
  headroomPercent: number | null;
}

/** The space used and left in every partition of a layout. */
export interface LayoutReport {
  flashSize: number;
  unallocated: number;
  partitions: PartitionReport[];
}

/** What `writeLayout` writes. */
export type LayoutFormat = "espressif" | "mbr" | "linker" | "constants";
