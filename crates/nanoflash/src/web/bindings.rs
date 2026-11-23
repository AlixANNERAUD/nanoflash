//! The Web Serial, stream and timer APIs the transport calls, declared by hand: `web-sys` gates
//! Web Serial behind unstable APIs and would pull in `std`.
//!
//! Every call that the browser may refuse by throwing is declared with `catch`, so that a thrown
//! exception becomes an `Err` instead of unwinding through WebAssembly. Types are cast with
//! `unchecked_into`: test doubles of these objects are plain JavaScript objects.

use js_sys::{Function, Object, Promise, Uint8Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// A `SerialPort` of Web Serial.
    pub type SerialPort;

    /// Opens the port with `{ baudRate, bufferSize }`.
    #[wasm_bindgen(method, catch)]
    pub fn open(this: &SerialPort, options: &Object) -> Result<Promise, JsValue>;

    /// Closes the port; fails while a stream of the port is locked.
    #[wasm_bindgen(method, catch)]
    pub fn close(this: &SerialPort) -> Result<Promise, JsValue>;

    /// Sets `{ dataTerminalReady, requestToSend }`.
    #[wasm_bindgen(method, catch, js_name = setSignals)]
    pub fn set_signals(this: &SerialPort, signals: &Object) -> Result<Promise, JsValue>;

    /// `{ usbVendorId, usbProductId }` for a USB device, else `{}`.
    #[wasm_bindgen(method, catch, js_name = getInfo)]
    pub fn get_info(this: &SerialPort) -> Result<JsValue, JsValue>;

    /// The stream of received bytes, `null` while the port is closed.
    #[wasm_bindgen(method, getter)]
    pub fn readable(this: &SerialPort) -> JsValue;

    /// The stream of bytes to send, `null` while the port is closed.
    #[wasm_bindgen(method, getter)]
    pub fn writable(this: &SerialPort) -> JsValue;

    /// A `ReadableStream`.
    pub type ReadableStream;

    /// Locks the stream to a new reader; fails when it is locked already.
    #[wasm_bindgen(method, catch, js_name = getReader)]
    pub fn get_reader(this: &ReadableStream) -> Result<Reader, JsValue>;

    /// A `ReadableStreamDefaultReader`.
    pub type Reader;

    /// The next chunk, as `{ value, done }`.
    #[wasm_bindgen(method, catch)]
    pub fn read(this: &Reader) -> Result<Promise, JsValue>;

    /// Cancels the stream; a pending read resolves with `done`.
    #[wasm_bindgen(method, catch)]
    pub fn cancel(this: &Reader) -> Result<Promise, JsValue>;

    /// Unlocks the stream.
    #[wasm_bindgen(method, catch, js_name = releaseLock)]
    pub fn release_lock(this: &Reader) -> Result<(), JsValue>;

    /// A `WritableStream`.
    pub type WritableStream;

    /// Locks the stream to a new writer; fails when it is locked already.
    #[wasm_bindgen(method, catch, js_name = getWriter)]
    pub fn get_writer(this: &WritableStream) -> Result<Writer, JsValue>;

    /// A `WritableStreamDefaultWriter`.
    pub type Writer;

    /// Writes one chunk.
    #[wasm_bindgen(method, catch)]
    pub fn write(this: &Writer, chunk: &Uint8Array) -> Result<Promise, JsValue>;

    /// Unlocks the stream.
    #[wasm_bindgen(method, catch, js_name = releaseLock)]
    pub fn release_lock(this: &Writer) -> Result<(), JsValue>;

    /// The ports the page was granted, as an array of `SerialPort`.
    #[wasm_bindgen(catch, js_namespace = ["navigator", "serial"], js_name = getPorts)]
    pub fn get_ports() -> Result<Promise, JsValue>;

    /// Calls `handler` after `milliseconds`; returns the timer.
    #[wasm_bindgen(js_name = setTimeout)]
    pub fn set_timeout(handler: &Function, milliseconds: i32) -> JsValue;

    /// Calls `handler` with `argument` after `milliseconds`; returns the timer.
    #[wasm_bindgen(js_name = setTimeout)]
    pub fn set_timeout_with(handler: &Function, milliseconds: i32, argument: &JsValue) -> JsValue;

    /// Cancels a timer of `setTimeout`.
    #[wasm_bindgen(js_name = clearTimeout)]
    pub fn clear_timeout(id: &JsValue);

    /// The string form of any value, as `String(value)` gives it.
    #[wasm_bindgen(catch, js_name = String)]
    pub fn string(value: &JsValue) -> Result<alloc::string::String, JsValue>;
}
