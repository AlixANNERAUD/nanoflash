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

/// How long one write may wait for the device to accept data.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// How many writes in a row may make no progress before the port is given up on.
const WRITE_STALLS: u32 = 5;

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
        NativeSerialPort::open_with_identity(path, baud_rate, identity_by_path(path))
    }

    fn open_with_identity(
        path: &str,
        baud_rate: u32,
        identity: Option<UsbIdentity>,
    ) -> Result<NativeSerialPort, TransportError> {
        let timeout = Duration::from_millis(100);
        let port = serialport::new(path, baud_rate)
            .timeout(timeout)
            .open()
            .map_err(|error| serial_error(path, &error))?;
        Ok(NativeSerialPort {
            port,
            path: path.to_string(),
            identity,
            timeout,
        })
    }

    #[cfg(all(test, unix))]
    fn from_port(port: Box<dyn serialport::SerialPort>, path: &str) -> NativeSerialPort {
        NativeSerialPort {
            port,
            path: path.to_string(),
            identity: None,
            timeout: Duration::from_millis(100),
        }
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        if timeout != self.timeout {
            self.port
                .set_timeout(timeout)
                .map_err(|error| serial_error(&self.path, &error))?;
            self.timeout = timeout;
        }
        Ok(())
    }

    /// The path the port was opened with.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Looks the identity up by path, after resolving links such as `/dev/serial/by-id/...`.
fn identity_by_path(path: &str) -> Option<UsbIdentity> {
    let ports = serialport::available_ports().ok()?;
    let wanted = std::fs::canonicalize(path).ok();
    ports.into_iter().find_map(|port| {
        let SerialPortType::UsbPort(usb) = &port.port_type else {
            return None;
        };
        // Names such as `COM3` cannot be canonicalized: compare them as written.
        let same = match (&wanted, std::fs::canonicalize(&port.port_name)) {
            (Some(wanted), Ok(candidate)) => *wanted == candidate,
            _ => port.port_name == path,
        };
        same.then(|| identity_of(usb))
    })
}

impl SerialPort for NativeSerialPort {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        self.set_timeout(timeout)?;
        // An interrupted call is retried, bounded like a stalled write.
        let mut interruptions = 0;
        loop {
            match self.port.read(buffer) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == ErrorKind::TimedOut => return Ok(0),
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    interruptions += 1;
                    if interruptions >= WRITE_STALLS {
                        return Err(io_error(&self.path, &error));
                    }
                }
                Err(error) => return Err(io_error(&self.path, &error)),
            }
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        // The timeout of the last read must not cut a large write short.
        self.set_timeout(WRITE_TIMEOUT)?;
        let mut rest = data;
        let mut stalls = 0;
        while !rest.is_empty() {
            match self.port.write(rest) {
                Ok(0) => {
                    return Err(TransportError::Io {
                        message: std::format!("{}: the port accepted no data", self.path),
                    });
                }
                Ok(count) => {
                    rest = rest.get(count..).unwrap_or_default();
                    stalls = 0;
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted) =>
                {
                    stalls += 1;
                    if stalls >= WRITE_STALLS {
                        return Err(io_error(&self.path, &error));
                    }
                }
                Err(error) => return Err(io_error(&self.path, &error)),
            }
        }
        self.port
            .flush()
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
        // A timeout too large for the clock means no deadline.
        let deadline = Instant::now().checked_add(timeout);
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
                    if let Ok(opened) = NativeSerialPort::open_with_identity(
                        &port.path,
                        baud_rate,
                        port.identity.clone(),
                    ) {
                        return Ok(opened);
                    }
                }
            }
            let pause = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(TransportError::NotFound {
                            filter: filter.to_string(),
                        });
                    }
                    POLL_INTERVAL.min(deadline - now)
                }
                None => POLL_INTERVAL,
            };
            std::thread::sleep(pause);
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

    #[cfg(unix)]
    #[test]
    fn a_pseudo_terminal_pair_round_trips() {
        let (master, slave) = serialport::TTYPort::pair().expect("a pty pair");
        let mut a = NativeSerialPort::from_port(Box::new(master), "master");
        let mut b = NativeSerialPort::from_port(Box::new(slave), "slave");
        pollster::block_on(async {
            a.write(&[1, 2, 3]).await.expect("write");
            let mut buffer = [0u8; 8];
            let mut received = 0;
            let deadline = Instant::now() + Duration::from_secs(2);
            while received < 3 && Instant::now() < deadline {
                received += b
                    .read(&mut buffer[received..], Duration::from_millis(100))
                    .await
                    .expect("read");
            }
            assert_eq!(&buffer[..received], &[1, 2, 3]);
            assert_eq!(b.read(&mut buffer, Duration::from_millis(50)).await, Ok(0));
            // A large write after a short read timeout is not cut short.
            let big = std::vec![7u8; 4096];
            let writer = std::thread::spawn(move || pollster::block_on(a.write(&big)).map(|()| a));
            let mut total = 0;
            let deadline = Instant::now() + Duration::from_secs(5);
            while total < 4096 && Instant::now() < deadline {
                total += b
                    .read(&mut buffer, Duration::from_millis(50))
                    .await
                    .expect("read");
            }
            assert_eq!(total, 4096);
            assert!(writer.join().expect("writer").is_ok());
            assert_eq!(b.identity(), None);
        });
    }
}
