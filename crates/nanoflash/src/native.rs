//! The serial transport of the host, over the `serialport` crate.
//!
//! These calls block: reading waits for the port timeout and sleeping parks the thread. That is
//! harmless on the command line's single-threaded executor. A library user in a multi-threaded
//! runtime runs the flashing on a dedicated thread.

use std::boxed::Box;
use std::io::{ErrorKind, Read, Write};
use std::string::{String, ToString};
use std::time::Instant;
use std::vec::Vec;

use serde::Serialize;
use serialport::{SerialPortType, UsbPortInfo};

use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};

/// How often the provider looks for a device that has not appeared yet.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A serial port of the host.
pub struct NativeSerialPort {
    port: Box<dyn serialport::SerialPort>,
    path: String,
    identity: Option<UsbIdentity>,
    timeout: Duration,
}

/// A serial port found on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortInformation {
    /// The device path (`/dev/ttyACM0`, `COM3`).
    pub path: String,
    /// The USB identity, for USB ports.
    pub identity: Option<UsbIdentity>,
    /// The product name the device reports, when it does.
    pub product: Option<String>,
}

fn identity_of(information: &UsbPortInfo) -> UsbIdentity {
    UsbIdentity {
        vendor: information.vid,
        product: information.pid,
        serial_number: information.serial_number.clone(),
    }
}

fn is_disconnection(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::BrokenPipe | ErrorKind::NotConnected | ErrorKind::UnexpectedEof
    ) || matches!(error.raw_os_error(), Some(5 | 6 | 19))
}

fn io_error(path: &str, error: &std::io::Error) -> TransportError {
    if is_disconnection(error) {
        TransportError::Disconnected
    } else {
        TransportError::Io {
            message: std::format!("{path}: {error}"),
        }
    }
}

fn serial_error(path: &str, error: &serialport::Error) -> TransportError {
    match error.kind() {
        serialport::ErrorKind::NoDevice => TransportError::Disconnected,
        serialport::ErrorKind::Io(kind) if is_disconnection(&std::io::Error::from(kind)) => {
            TransportError::Disconnected
        }
        _ => TransportError::Io {
            message: std::format!("{path}: {error}"),
        },
    }
}

/// The serial ports of the host.
pub fn list_ports() -> Result<Vec<PortInformation>, TransportError> {
    let ports = serialport::available_ports().map_err(|error| TransportError::Io {
        message: std::format!("cannot list the serial ports: {error}"),
    })?;
    Ok(ports
        .into_iter()
        .map(|port| {
            let (identity, product) = match &port.port_type {
                SerialPortType::UsbPort(usb) => (Some(identity_of(usb)), usb.product.clone()),
                _ => (None, None),
            };
            PortInformation {
                path: port.port_name,
                identity,
                product,
            }
        })
        .collect())
}

impl NativeSerialPort {
    /// Opens the port at `path` with 8 data bits, no parity and one stop bit.
    pub fn open(path: &str, baud_rate: u32) -> Result<NativeSerialPort, TransportError> {
        let timeout = Duration::from_millis(100);
        let port = serialport::new(path, baud_rate)
            .timeout(timeout)
            .open()
            .map_err(|error| serial_error(path, &error))?;
        Ok(NativeSerialPort {
            port,
            path: path.to_string(),
            identity: identity_by_path(path),
            timeout,
        })
    }

    /// The path the port was opened with.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Looks the identity up by path, after resolving links such as `/dev/serial/by-id/...`.
fn identity_by_path(path: &str) -> Option<UsbIdentity> {
    let wanted = std::fs::canonicalize(path).ok()?;
    let ports = serialport::available_ports().ok()?;
    ports.into_iter().find_map(|port| {
        let SerialPortType::UsbPort(usb) = &port.port_type else {
            return None;
        };
        let candidate = std::fs::canonicalize(&port.port_name).ok()?;
        (candidate == wanted).then(|| identity_of(usb))
    })
}

impl SerialPort for NativeSerialPort {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        if timeout != self.timeout {
            self.port
                .set_timeout(timeout)
                .map_err(|error| serial_error(&self.path, &error))?;
            self.timeout = timeout;
        }
        match self.port.read(buffer) {
            Ok(count) => Ok(count),
            Err(error) if error.kind() == ErrorKind::TimedOut => Ok(0),
            Err(error) => Err(io_error(&self.path, &error)),
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.port
            .write_all(data)
            .and_then(|()| self.port.flush())
            .map_err(|error| io_error(&self.path, &error))
    }

    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError> {
        self.port
            .set_baud_rate(baud_rate)
            .map_err(|error| serial_error(&self.path, &error))
    }

    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError> {
        // DTR first, then RTS: Windows only propagates DTR on a change of RTS.
        self.port
            .write_data_terminal_ready(signals.dtr)
            .map_err(|error| serial_error(&self.path, &error))?;
        self.port
            .write_request_to_send(signals.rts)
            .map_err(|error| serial_error(&self.path, &error))
    }

    async fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn identity(&self) -> Option<UsbIdentity> {
        self.identity.clone()
    }
}

/// Finds ports of the host by their USB identity.
#[derive(Default)]
pub struct NativePortProvider;

impl PortProvider for NativePortProvider {
    type Port = NativeSerialPort;

    async fn open(
        &mut self,
        filter: &DeviceFilter,
        baud_rate: u32,
        timeout: Duration,
    ) -> Result<NativeSerialPort, TransportError> {
        let deadline = Instant::now() + timeout;
        loop {
            // Listing and opening errors are retried: the device may still be appearing.
            if let Ok(ports) = list_ports() {
                for port in ports {
                    let selected = port
                        .identity
                        .as_ref()
                        .is_some_and(|identity| filter.matches(identity));
                    if !selected {
                        continue;
                    }
                    if let Ok(opened) = NativeSerialPort::open(&port.path, baud_rate) {
                        return Ok(opened);
                    }
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(TransportError::NotFound {
                    filter: filter.to_string(),
                });
            }
            std::thread::sleep(POLL_INTERVAL.min(deadline - now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_a_usb_port() {
        let information = serialport::UsbPortInfo {
            vid: 0x303a,
            pid: 0x1001,
            serial_number: Some("ab".into()),
            manufacturer: None,
            product: None,
        };
        assert_eq!(
            identity_of(&information),
            crate::transport::UsbIdentity {
                vendor: 0x303a,
                product: 0x1001,
                serial_number: Some("ab".into())
            }
        );
    }

    #[test]
    fn opening_a_missing_port_names_it() {
        let error = NativeSerialPort::open("/dev/nanoflash-missing", 115200).err();
        match error {
            Some(crate::transport::TransportError::Io { message }) => {
                assert!(message.contains("/dev/nanoflash-missing"), "{message}")
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
