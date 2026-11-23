//! The serial transport of the browser, over Web Serial (Chromium-based browsers).
//!
//! A [`WebSerialPort`] wraps a `SerialPort` the page obtained from `navigator.serial`; a
//! [`WebPortProvider`] finds a device that came back as a new port among the ports the page was
//! granted. Both stay `no_std`: time is measured by `setTimeout`, not by a clock.

pub mod bindings;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};

/// The receive buffer of the browser, large enough for a whole flash block.
const BUFFER_SIZE: u32 = 65536;

/// How long to wait before opening again a port that is still open or closing.
const OPEN_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// How many times to open again a port that is still open or closing.
const OPEN_RETRIES: u32 = 10;

/// How often the provider looks for a device that has not appeared yet.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// An object with the given properties.
fn object(properties: &[(&str, JsValue)]) -> Object {
    let object = Object::new();
    for (name, value) in properties {
        // Setting a property of a fresh plain object cannot fail.
        let _ = Reflect::set(&object, &JsValue::from_str(name), value);
    }
    object
}

/// A property of `value`, `undefined` when it has none or is not an object.
fn property(value: &JsValue, name: &str) -> JsValue {
    if !value.is_object() {
        return JsValue::UNDEFINED;
    }
    Reflect::get(value, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

/// The `name` of a JavaScript error, such as `NetworkError`.
fn error_name(error: &JsValue) -> Option<String> {
    property(error, "name").as_string()
}

/// What a JavaScript error says: its `message`, else its string form.
pub fn describe(error: &JsValue) -> String {
    if let Some(message) = property(error, "message").as_string() {
        return message;
    }
    if let Some(text) = error.as_string() {
        return text;
    }
    bindings::string(error).unwrap_or_else(|_| "an unknown error".to_string())
}

/// The transport error for a rejection of Web Serial: a lost device (`NetworkError`) is
/// `Disconnected`.
fn transport_error(error: &JsValue) -> TransportError {
    match error_name(error).as_deref() {
        Some("NetworkError") => TransportError::Disconnected,
        _ => TransportError::Io {
            message: describe(error),
        },
    }
}

/// Waits for the promise a call returned, or gives what it threw.
async fn settle(promise: Result<Promise, JsValue>) -> Result<JsValue, JsValue> {
    JsFuture::from(promise?).await
}

/// Lets a promise settle unobserved, without an unhandled rejection.
fn ignore(promise: Result<Promise, JsValue>) {
    if let Ok(promise) = promise {
        // The handlers the future attaches outlive it, and swallow the outcome.
        drop(JsFuture::from(promise));
    }
}

/// A promise that resolves to `undefined` after `milliseconds`, and its timer.
fn timer(milliseconds: i32) -> (Promise, JsValue) {
    let mut id = JsValue::UNDEFINED;
    let promise = Promise::new(&mut |resolve, _reject| {
        id = bindings::set_timeout(&resolve, milliseconds);
    });
    (promise, id)
}

/// The milliseconds of `duration`, at most `i32::MAX` (24 days) as `setTimeout` takes.
fn milliseconds(duration: Duration) -> i32 {
    i32::try_from(duration.as_millis()).unwrap_or(i32::MAX)
}

/// Waits for `duration`.
pub async fn sleep(duration: Duration) {
    let (promise, _) = timer(milliseconds(duration));
    // The promise only resolves.
    let _ = JsFuture::from(promise).await;
}

/// A USB identifier from `getInfo()`: a whole number of 16 bits.
fn identifier(value: &JsValue) -> Option<u16> {
    let number = value.as_f64()?;
    if !(0.0..=f64::from(u16::MAX)).contains(&number) {
        return None;
    }
    // In range, so the conversion only drops a fraction, which makes the identifier invalid.
    let identifier = number as u16;
    (f64::from(identifier) == number).then_some(identifier)
}

/// The USB identity `getInfo()` gives, when it has both identifiers. Web Serial does not give
/// the serial number.
fn identity_of(port: &bindings::SerialPort) -> Option<UsbIdentity> {
    let information = port.get_info().ok()?;
    Some(UsbIdentity {
        vendor: identifier(&property(&information, "usbVendorId"))?,
        product: identifier(&property(&information, "usbProductId"))?,
        serial_number: None,
    })
}

/// Opens `port` at `baud_rate`, again while it is still open or closing (`InvalidStateError`).
async fn open_port(port: &bindings::SerialPort, baud_rate: u32) -> Result<(), TransportError> {
    let options = object(&[
        ("baudRate", JsValue::from(baud_rate)),
        ("bufferSize", JsValue::from(BUFFER_SIZE)),
    ]);
    let mut retries = 0;
    loop {
        match settle(port.open(&options)).await {
            Ok(_) => return Ok(()),
            Err(error)
                if retries < OPEN_RETRIES
                    && error_name(&error).as_deref() == Some("InvalidStateError") =>
            {
                retries += 1;
                sleep(OPEN_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(transport_error(&error)),
        }
    }
}

/// Locks the streams of an open port.
fn lock_streams(
    port: &bindings::SerialPort,
) -> Result<(bindings::Reader, bindings::Writer), TransportError> {
    let closed = || TransportError::Io {
        message: "the port has no stream: it is not open".to_string(),
    };
    let readable = port.readable();
    let writable = port.writable();
    if !readable.is_object() || !writable.is_object() {
        return Err(closed());
    }
    let readable: bindings::ReadableStream = readable.unchecked_into();
    let writable: bindings::WritableStream = writable.unchecked_into();
    let reader = readable
        .get_reader()
        .map_err(|error| transport_error(&error))?;
    match writable.get_writer() {
        Ok(writer) => Ok((reader, writer)),
        Err(error) => {
            let _ = reader.release_lock();
            Err(transport_error(&error))
        }
    }
}

/// A Web Serial port, open, with its streams locked.
///
/// A chunk larger than the buffer of a read is kept for the next reads, and a read that times
/// out keeps its pending `reader.read()` for the next one: no byte is lost or reordered.
pub struct WebSerialPort {
    port: bindings::SerialPort,
    reader: Option<bindings::Reader>,
    writer: Option<bindings::Writer>,
    /// A `reader.read()` that lost the race against a timeout.
    pending: Option<Promise>,
    /// The bytes of the last chunk that did not fit in the buffer of the read.
    leftover: Option<Uint8Array>,
    identity: Option<UsbIdentity>,
}

impl WebSerialPort {
    /// Opens `port`, a `SerialPort` of Web Serial, at `baud_rate`, and locks its streams.
    pub async fn open(port: JsValue, baud_rate: u32) -> Result<WebSerialPort, TransportError> {
        let port: bindings::SerialPort = port.unchecked_into();
        open_port(&port, baud_rate).await?;
        let (reader, writer) = match lock_streams(&port) {
            Ok(streams) => streams,
            Err(error) => {
                ignore(port.close());
                return Err(error);
            }
        };
        let identity = identity_of(&port);
        Ok(WebSerialPort {
            port,
            reader: Some(reader),
            writer: Some(writer),
            pending: None,
            leftover: None,
            identity,
        })
    }

    /// Releases the streams and closes the port.
    pub async fn close(mut self) -> Result<(), TransportError> {
        self.release().await;
        settle(self.port.close())
            .await
            .map(drop)
            .map_err(|error| transport_error(&error))
    }

    /// The `SerialPort` of Web Serial.
    pub fn handle(&self) -> JsValue {
        JsValue::clone(&self.port)
    }

    /// Cancels the reader and waits for the cancellation, forgets what was read, and unlocks
    /// both streams.
    async fn release(&mut self) {
        self.pending = None;
        self.leftover = None;
        if let Some(reader) = self.reader.take() {
            // A failed cancellation leaves the stream locked: closing reports it.
            let _ = settle(reader.cancel()).await;
            let _ = reader.release_lock();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.release_lock();
        }
    }

    /// Copies the start of `chunk` into `buffer` and keeps the rest.
    fn take_from(&mut self, chunk: Uint8Array, buffer: &mut [u8]) -> usize {
        let length = chunk.length();
        let count = u32::try_from(buffer.len()).unwrap_or(u32::MAX).min(length);
        // `count` is at most `buffer.len()`.
        let Some(destination) = buffer.get_mut(..count as usize) else {
            return 0;
        };
        chunk.subarray(0, count).copy_to(destination);
        if count < length {
            self.leftover = Some(chunk.subarray(count, length));
        }
        destination.len()
    }

    fn not_open() -> TransportError {
        TransportError::Io {
            message: "the port is not open".to_string(),
        }
    }
}

impl Drop for WebSerialPort {
    /// Cancels a pending read and unlocks both streams, so that the port can be closed.
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            if self.pending.take().is_some() {
                ignore(reader.cancel());
            }
            let _ = reader.release_lock();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.release_lock();
        }
    }
}

impl SerialPort for WebSerialPort {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        if let Some(leftover) = self.leftover.take() {
            return Ok(self.take_from(leftover, buffer));
        }
        let pending = match self.pending.take() {
            Some(pending) => pending,
            None => {
                let reader = self.reader.as_ref().ok_or_else(WebSerialPort::not_open)?;
                reader.read().map_err(|error| transport_error(&error))?
            }
        };
        // Kept until it settles, so that a read dropped while waiting loses nothing.
        self.pending = Some(pending.clone());
        let (timer, id) = timer(milliseconds(timeout));
        let outcome = JsFuture::from(Promise::race(&Array::of2(&pending, &timer))).await;
        // The timer resolves to `undefined`, a read to `{ value, done }`.
        if matches!(&outcome, Ok(result) if result.is_undefined()) {
            return Ok(0);
        }
        self.pending = None;
        bindings::clear_timeout(&id);
        let result = outcome.map_err(|error| transport_error(&error))?;
        if property(&result, "done").as_bool() == Some(true) {
            return Err(TransportError::Disconnected);
        }
        let chunk = property(&result, "value")
            .dyn_into::<Uint8Array>()
            .map_err(|_| TransportError::Io {
                message: "the port gave a chunk that is not a Uint8Array".to_string(),
            })?;
        Ok(self.take_from(chunk, buffer))
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let writer = self.writer.as_ref().ok_or_else(WebSerialPort::not_open)?;
        // A copy: the memory of the module may move while the write is pending.
        let chunk = Uint8Array::from(data);
        settle(writer.write(&chunk))
            .await
            .map(drop)
            .map_err(|error| transport_error(&error))
    }

    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError> {
        // Web Serial has no baud rate change: close the port and open it again.
        self.release().await;
        settle(self.port.close())
            .await
            .map_err(|error| transport_error(&error))?;
        open_port(&self.port, baud_rate).await?;
        let (reader, writer) = lock_streams(&self.port)?;
        self.reader = Some(reader);
        self.writer = Some(writer);
        Ok(())
    }

    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError> {
        let signals = object(&[
            ("dataTerminalReady", JsValue::from_bool(signals.dtr)),
            ("requestToSend", JsValue::from_bool(signals.rts)),
        ]);
        settle(self.port.set_signals(&signals))
            .await
            .map(drop)
            .map_err(|error| transport_error(&error))
    }

    async fn sleep(&mut self, duration: Duration) {
        sleep(duration).await;
    }

    fn identity(&self) -> Option<UsbIdentity> {
        self.identity.clone()
    }
}

/// Opens a device again among the ports the page was granted (`navigator.serial.getPorts()`),
/// and remembers the ports it opened, for the caller to close them.
#[derive(Default)]
pub struct WebPortProvider {
    opened: Vec<JsValue>,
}

impl WebPortProvider {
    /// The `SerialPort` of every port this provider opened.
    pub fn opened(&self) -> Vec<JsValue> {
        self.opened.clone()
    }

    /// Opens the first granted port that `filter` selects, if any.
    async fn open_granted(
        &mut self,
        filter: &DeviceFilter,
        baud_rate: u32,
    ) -> Option<WebSerialPort> {
        // Listing and opening errors are retried: the device may still be appearing.
        let ports = settle(bindings::get_ports()).await.ok()?;
        for port in Array::from(&ports).iter() {
            let handle: bindings::SerialPort = port.clone().unchecked_into();
            let selected = identity_of(&handle).is_some_and(|identity| filter.matches(&identity));
            if !selected {
                continue;
            }
            if let Ok(opened) = WebSerialPort::open(port, baud_rate).await {
                self.opened.push(opened.handle());
                return Some(opened);
            }
        }
        None
    }
}

impl PortProvider for WebPortProvider {
    type Port = WebSerialPort;

    async fn open(
        &mut self,
        filter: &DeviceFilter,
        baud_rate: u32,
        timeout: Duration,
    ) -> Result<WebSerialPort, TransportError> {
        // Counted in polls: the library has no clock.
        let polls = timeout.as_millis() / POLL_INTERVAL.as_millis();
        let mut poll = 0;
        loop {
            if let Some(port) = self.open_granted(filter, baud_rate).await {
                return Ok(port);
            }
            if poll >= polls {
                // Web Serial only lists the ports the user granted: a device that came back
                // under another identity needs to be selected once.
                return Err(TransportError::NotGranted {
                    filter: filter.to_string(),
                });
            }
            poll += 1;
            sleep(POLL_INTERVAL).await;
        }
    }
}
