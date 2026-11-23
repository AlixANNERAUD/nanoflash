//! The WebAssembly modules of the nanoflash web package.
//!
//! Every module exports `checkLayout` and `writeLayout`; with the feature of a backend
//! (`espressif`, `samba`, or `avr` for STK500v1, STK500v2 and AVR109), it also exports `flash`
//! and `detect`, which run that backend over Web Serial. Values cross as plain JavaScript
//! objects in the serde form of the library (snake_case fields, kebab-case `kind`). A failure is
//! thrown as the serde form of a [`nanoflash::Error`] with its `message` (the text to show) and
//! its `class` (`input`, `device` or `verification`); a malformed argument of these functions is
//! `{ kind: "invalid-argument", message, class: "input" }`.

#![forbid(unsafe_code)]

use js_sys::JSON;
use nanoflash::Error;
use nanoflash::layout::{ImageSize, Layout, LayoutErrors, ResolvedLayout};
use nanoflash::table::{espressif, linker, mbr};
use serde::Serialize;
use serde_json::{Map, Value};
use wasm_bindgen::prelude::*;

/// A JSON value as a JavaScript value.
fn to_js(value: &Value) -> JsValue {
    // The text of a `Value` is always valid JSON.
    JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL)
}

/// A serializable value as a JavaScript value.
fn serialized(value: &impl Serialize) -> JsValue {
    to_js(&serde_json::to_value(value).unwrap_or(Value::Null))
}

/// An error of the library as the JavaScript value thrown: its serde form, its `message` and its
/// `class`. A variant with a `message` field of its own takes the full text instead, which
/// includes it.
fn failure(error: &Error) -> JsValue {
    let mut fields = match serde_json::to_value(error) {
        Ok(Value::Object(fields)) => fields,
        _ => Map::new(),
    };
    fields.insert("message".into(), Value::String(error.to_string()));
    fields.insert(
        "class".into(),
        serde_json::to_value(error.class()).unwrap_or(Value::Null),
    );
    to_js(&Value::Object(fields))
}

/// A malformed argument, such as options that are not the expected JSON.
fn invalid_argument(message: String) -> JsValue {
    let mut fields = Map::new();
    fields.insert("kind".into(), Value::String("invalid-argument".into()));
    fields.insert("message".into(), Value::String(message));
    fields.insert("class".into(), Value::String("input".into()));
    to_js(&Value::Object(fields))
}

/// Reads and resolves the layout file `text`, named `name` in messages.
fn resolve(text: &str, name: &str) -> Result<ResolvedLayout, JsValue> {
    let layout = Layout::from_toml(text).map_err(|error| {
        failure(&Error::LayoutFile {
            path: name.into(),
            message: error.message,
        })
    })?;
    layout
        .resolve()
        .map_err(|LayoutErrors(errors)| failure(&Error::Layout { errors }))
}

/// Checks the layout file `text` (TOML), named `name` in messages, with the sizes of the images
/// in `images` (JSON `[{ "partition": "application", "size": 1024 }]`); returns the report of
/// every partition.
#[wasm_bindgen(js_name = checkLayout)]
pub fn check_layout(text: &str, images: &str, name: &str) -> Result<JsValue, JsValue> {
    let resolved = resolve(text, name)?;
    let images: Vec<ImageSize> = serde_json::from_str(images).map_err(|error| {
        invalid_argument(format!(
            "the image sizes are not a JSON array of {{\"partition\", \"size\"}}: {error}"
        ))
    })?;
    let report = resolved
        .report(&images)
        .map_err(|LayoutErrors(errors)| failure(&Error::Layout { errors }))?;
    Ok(serialized(&report))
}

/// Writes what `format` names for the layout file `text` (TOML), named `name` in messages: the
/// `espressif` or `mbr` partition table, the `linker` memory regions or the Rust `constants`.
#[wasm_bindgen(js_name = writeLayout)]
pub fn write_layout(text: &str, format: &str, name: &str) -> Result<Vec<u8>, JsValue> {
    if !matches!(format, "espressif" | "mbr" | "linker" | "constants") {
        return Err(invalid_argument(format!(
            "unknown layout output \"{format}\": expected espressif, mbr, linker or constants"
        )));
    }
    let resolved = resolve(text, name)?;
    match format {
        "espressif" => espressif::write(&resolved),
        "mbr" => mbr::write(&resolved).map(|table| table.to_vec()),
        "linker" => linker::memory_regions(&resolved).map(String::into_bytes),
        _ => linker::rust_constants(&resolved).map(String::into_bytes),
    }
    .map_err(|error| failure(&Error::Table { error }))
}

#[cfg(any(feature = "espressif", feature = "samba", feature = "avr"))]
mod device {
    //! Flashing and detecting over Web Serial.

    use js_sys::{ArrayBuffer, Function, Object, Promise, Reflect, Uint8Array};
    use nanoflash::Error;
    use nanoflash::backend::{Progress, ProgressEvent};
    use nanoflash::flash::{BackendOptions, FlashOptions, FlashReport, Hooks, ImageSource};
    use nanoflash::manifest::{Manifest, Protocol};
    use nanoflash::web::{WebPortProvider, WebSerialPort, close_port};
    use serde::Deserialize;
    use serde_json::Value;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    use super::{failure, invalid_argument, serialized, to_js};

    #[wasm_bindgen]
    extern "C" {
        /// The string form of any value, as `String(value)` gives it.
        #[wasm_bindgen(catch, js_name = String)]
        fn string(value: &JsValue) -> Result<String, JsValue>;
    }

    /// What a JavaScript error says: its `message`, else its string form.
    fn describe(error: &JsValue) -> String {
        let message = if error.is_object() {
            Reflect::get(error, &"message".into()).ok()
        } else {
            None
        };
        if let Some(message) = message.and_then(|message| message.as_string()) {
            return message;
        }
        if let Some(text) = error.as_string() {
            return text;
        }
        string(error).unwrap_or_else(|_| "an unknown error".to_string())
    }

    /// The baud rate the port is opened at: the one Espressif's ROM loaders start at, which
    /// Arduino's SAM-BA bootloaders also use.
    const INITIAL_BAUD_RATE: u32 = 115_200;

    /// The options of `flash`, as JSON.
    #[derive(Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct WebFlashOptions {
        board_identifier: Option<String>,
        erase_all: bool,
        verify: bool,
        baud_rate: Option<u32>,
    }

    impl Default for WebFlashOptions {
        fn default() -> Self {
            WebFlashOptions {
                board_identifier: None,
                erase_all: false,
                verify: true,
                baud_rate: None,
            }
        }
    }

    /// The options of `detect`, as JSON. Detecting keeps the baud rate of the bootloader, so
    /// there is none to choose.
    #[derive(Default, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct WebDetectOptions {
        protocol: Option<Protocol>,
    }

    fn options<T: for<'de> Deserialize<'de>>(text: &str, function: &str) -> Result<T, JsValue> {
        serde_json::from_str(text).map_err(|error| {
            invalid_argument(format!("the options of {function} are invalid: {error}"))
        })
    }

    /// Reads the files of a manifest through a JavaScript function `path => Promise<Uint8Array>`.
    struct JsImageSource {
        read_file: Function,
    }

    impl ImageSource for JsImageSource {
        async fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
            let returned = self
                .read_file
                .call1(&JsValue::NULL, &JsValue::from_str(path))
                .map_err(|error| describe(&error))?;
            let value = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|error| describe(&error))?;
            let bytes = if value.is_instance_of::<ArrayBuffer>() {
                Uint8Array::new(&value)
            } else {
                value
                    .dyn_into::<Uint8Array>()
                    .map_err(|_| "the file was not read as a Uint8Array".to_string())?
            };
            let length = bytes.length() as usize;
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(length)
                .map_err(|_| format!("there is not enough memory for its {length} bytes"))?;
            // Within the capacity just reserved: no allocation.
            buffer.resize(length, 0);
            bytes.copy_to(&mut buffer);
            Ok(buffer)
        }
    }

    /// Calls a JavaScript function with `{ stage, offset, done, total }` for every progress event
    /// and `{ message }` for every message. What the function throws is ignored.
    struct JsProgress {
        callback: Function,
    }

    impl Progress for JsProgress {
        fn report(&mut self, event: ProgressEvent) {
            let _ = self.callback.call1(&JsValue::NULL, &serialized(&event));
        }

        fn message(&mut self, text: &str) {
            let message = Object::new();
            let _ = Reflect::set(&message, &"message".into(), &JsValue::from_str(text));
            let _ = self.callback.call1(&JsValue::NULL, &message);
        }
    }

    /// Opens `port`, the `SerialPort` given by the page.
    async fn open(port: &JsValue) -> Result<WebSerialPort, Error> {
        WebSerialPort::open(port.clone(), INITIAL_BAUD_RATE)
            .await
            .map_err(|error| Error::from(error).during(|| "opening the port".into()))
    }

    /// Closes the port of the page and every port the provider opened, whatever their state: the
    /// next attempt must find them closed.
    async fn close(port: &JsValue, provider: &WebPortProvider) {
        for handle in core::iter::once(port.clone()).chain(provider.opened()) {
            // Closing waits a few seconds at most; a port already closed refuses, which is fine.
            let _ = close_port(&handle).await;
        }
    }

    /// The report of a flash, each warning with its `message`.
    fn report(report: &FlashReport) -> JsValue {
        let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
        if let Some(Value::Array(warnings)) = value.get_mut("warnings") {
            for (warning, source) in warnings.iter_mut().zip(&report.warnings) {
                if let Value::Object(fields) = warning {
                    fields.insert("message".into(), Value::String(source.to_string()));
                }
            }
        }
        to_js(&value)
    }

    /// Flashes the board of `manifest` (JSON text) that the device on `port` is.
    ///
    /// `options` is `{"board_identifier": null, "erase_all": false, "verify": true,
    /// "baud_rate": null}`; `read_file(path)` gives the bytes of a file the manifest names, as
    /// written there; `progress` receives the events and messages. Returns the report of the
    /// flash. The port and every port opened on the way are closed before returning.
    #[wasm_bindgen]
    pub async fn flash(
        manifest: String,
        port: JsValue,
        options: String,
        read_file: Function,
        progress: Function,
    ) -> Result<JsValue, JsValue> {
        let manifest =
            Manifest::parse(&manifest).map_err(|error| failure(&Error::Manifest { error }))?;
        let options: WebFlashOptions = self::options(&options, "flash")?;
        let options = FlashOptions {
            board_identifier: options.board_identifier,
            erase_all: options.erase_all,
            verify: options.verify,
            backend: BackendOptions {
                baud_rate: options.baud_rate,
                ..BackendOptions::default()
            },
        };
        let mut provider = WebPortProvider::new().await;
        let mut source = JsImageSource { read_file };
        let mut progress = JsProgress { callback: progress };
        let outcome = async {
            let opened = open(&port).await?;
            let hooks = Hooks {
                source: &mut source,
                verifier: &mut (),
                progress: &mut progress,
            };
            nanoflash::flash::flash(&manifest, opened, &mut provider, hooks, &options).await
        }
        .await;
        close(&port, &provider).await;
        outcome
            .map(|flashed| report(&flashed))
            .map_err(|error| failure(&error))
    }

    /// Tells what chip the device on `port` is, then resets it into its application.
    ///
    /// `options` is `{"protocol": null}`, or a protocol such as `"espressif"` when the USB
    /// identity of the port does not tell it. Returns the chip information. The port and every
    /// port opened on the way are closed before returning.
    #[wasm_bindgen]
    pub async fn detect(
        port: JsValue,
        options: String,
        progress: Function,
    ) -> Result<JsValue, JsValue> {
        let options: WebDetectOptions = self::options(&options, "detect")?;
        let mut provider = WebPortProvider::new().await;
        let mut progress = JsProgress { callback: progress };
        let outcome = async {
            let opened = open(&port).await?;
            nanoflash::flash::detect(opened, &mut provider, options.protocol, &mut progress).await
        }
        .await;
        close(&port, &provider).await;
        outcome
            .map(|chip| serialized(&chip))
            .map_err(|error| failure(&error))
    }
}
