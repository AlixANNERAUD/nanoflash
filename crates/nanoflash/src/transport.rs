//! The serial transport a backend talks through, and the errors it can report.
//!
//! A [`SerialPort`] is one open port; a [`PortProvider`] finds or reopens a port for resets that
//! make the device appear again as a new one.

use alloc::string::String;
use core::fmt;

use serde::{Deserialize, Serialize};

pub use core::time::Duration;

/// The state of the modem control lines. `true` means asserted, as `serialport`'s
/// `write_data_terminal_ready(true)` and Web Serial's `dataTerminalReady: true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Signals {
    /// Data terminal ready.
    pub dtr: bool,
    /// Request to send.
    pub rts: bool,
}

/// The USB identity of a serial device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbIdentity {
    /// The USB vendor identifier.
    pub vendor: u16,
    /// The USB product identifier.
    pub product: u16,
    /// The serial number, when the device has one.
    pub serial_number: Option<String>,
}

/// Selects the devices a provider may open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawDeviceFilter")]
pub struct DeviceFilter {
    /// The USB vendor identifier.
    pub vendor: u16,
    /// The USB product identifier; any product when `None`.
    pub product: Option<u16>,
}

/// A filter as written in a manifest: integers or text for the identifiers.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeviceFilter {
    #[serde(deserialize_with = "crate::quantity::deserialize")]
    vendor: u64,
    #[serde(default, deserialize_with = "crate::quantity::deserialize_optional")]
    product: Option<u64>,
}

impl TryFrom<RawDeviceFilter> for DeviceFilter {
    type Error = String;

    fn try_from(raw: RawDeviceFilter) -> Result<Self, String> {
        let vendor = u16::try_from(raw.vendor).map_err(|_| {
            alloc::format!(
                "the USB vendor identifier 0x{:x} does not fit in 16 bits (at most 0xffff)",
                raw.vendor
            )
        })?;
        let product = match raw.product {
            None => None,
            Some(product) => Some(u16::try_from(product).map_err(|_| {
                alloc::format!(
                    "the USB product identifier 0x{product:x} does not fit in 16 bits (at most 0xffff)"
                )
            })?),
        };
        Ok(DeviceFilter { vendor, product })
    }
}

impl DeviceFilter {
    /// Whether `identity` is selected by this filter.
    pub fn matches(&self, identity: &UsbIdentity) -> bool {
        self.vendor == identity.vendor
            && self
                .product
                .is_none_or(|product| product == identity.product)
    }

    /// The filter selecting exactly the vendor and product of `identity`.
    pub fn exact(identity: &UsbIdentity) -> DeviceFilter {
        DeviceFilter {
            vendor: identity.vendor,
            product: Some(identity.product),
        }
    }
}

impl fmt::Display for DeviceFilter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.product {
            Some(product) => write!(formatter, "{:04x}:{:04x}", self.vendor, product),
            None => write!(formatter, "{:04x}:*", self.vendor),
        }
    }
}

/// What can go wrong when talking to a serial port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TransportError {
    /// The operating system or the browser reported an error.
    Io {
        /// What it reported.
        message: String,
    },
    /// The device went away.
    Disconnected,
    /// No device matches the filter.
    NotFound {
        /// The filter, as `303a:1001`.
        filter: String,
    },
    /// The browser has no permission for a device matching the filter.
    NotGranted {
        /// The filter, as `303a:1001`.
        filter: String,
    },
    /// A replayed transcript diverged from what the host did.
    Replay {
        /// The transcript line that diverged.
        line: usize,
        /// How it diverged.
        message: String,
    },
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io { message } => write!(formatter, "serial port error: {message}"),
            TransportError::Disconnected => formatter.write_str("the device disconnected"),
            TransportError::NotFound { filter } => {
                write!(formatter, "no device matching {filter} was found")
            }
            TransportError::NotGranted { filter } => write!(
                formatter,
                "the browser has no permission for a device matching {filter}; select it once"
            ),
            TransportError::Replay { line, message } => {
                write!(formatter, "transcript line {line}: {message}")
            }
        }
    }
}

impl core::error::Error for TransportError {}

/// One open serial port.
// Futures are not Send: browser APIs are single-threaded (docs/design.md, Constraints).
#[allow(async_fn_in_trait)]
pub trait SerialPort {
    /// Waits up to `timeout` for data; returns 0 when nothing arrived.
    async fn read(&mut self, buffer: &mut [u8], timeout: Duration)
    -> Result<usize, TransportError>;

    /// Writes all of `data`.
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError>;

    /// Changes the baud rate.
    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError>;

    /// Sets the DTR and RTS lines.
    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError>;

    /// Waits for `duration`.
    async fn sleep(&mut self, duration: Duration);

    /// The USB identity of the device, when it is a USB device.
    fn identity(&self) -> Option<UsbIdentity>;
}

/// Finds or reopens a port, for resets that re-enumerate (1200-baud touch, USB-Serial-JTAG).
// Futures are not Send: browser APIs are single-threaded (docs/design.md, Constraints).
#[allow(async_fn_in_trait)]
pub trait PortProvider {
    /// The kind of port this provider opens.
    type Port: SerialPort;

    /// Opens a device matching `filter`, waiting up to `timeout` for it to appear.
    async fn open(
        &mut self,
        filter: &DeviceFilter,
        baud_rate: u32,
        timeout: Duration,
    ) -> Result<Self::Port, TransportError>;
}
