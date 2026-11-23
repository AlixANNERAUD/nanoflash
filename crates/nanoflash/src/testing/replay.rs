//! A port that follows a transcript and reports where the host diverges from it.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::transcript::{Entry, Event, HostToken, Transcript};
use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};

struct State {
    events: Vec<Entry>,
    /// The index of the next event.
    next: usize,
    /// Within a `>` event: the token the next byte is compared with.
    host_position: usize,
    /// Within a `..` token: how many bytes it has taken.
    wildcard_count: usize,
    /// Within a `<` event: how many bytes were read.
    device_offset: usize,
}

/// Compares one written byte with the tokens of a line; returns the new position and count.
fn step(
    tokens: &[HostToken],
    position: usize,
    count: usize,
    byte: u8,
) -> Result<(usize, usize), String> {
    match tokens.get(position) {
        Some(HostToken::Byte(expected)) if *expected == byte => Ok((position + 1, 0)),
        Some(HostToken::Byte(expected)) => Err(format!("expected {expected:02x}, got {byte:02x}")),
        Some(HostToken::Any) => match tokens.get(position + 1) {
            Some(HostToken::Byte(next)) if count > 0 && *next == byte => Ok((position + 2, 0)),
            _ => Ok((position, count.saturating_add(1))),
        },
        None => Err(format!("expected no more bytes, got {byte:02x}")),
    }
}

impl State {
    fn advance(&mut self) {
        self.next += 1;
        self.host_position = 0;
        self.wildcard_count = 0;
        self.device_offset = 0;
    }

    fn replay_error(&self, message: String) -> TransportError {
        let line = self
            .events
            .get(self.next)
            .or(self.events.last())
            .map_or(0, |entry| entry.line);
        TransportError::Replay { line, message }
    }

    fn unexpected(&self, got: &str) -> TransportError {
        match self.events.get(self.next) {
            Some(entry) => TransportError::Replay {
                line: entry.line,
                message: format!("expected {}, got {got}", entry.event),
            },
            None => self.replay_error(format!("expected the end of the transcript, got {got}")),
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        for &byte in data {
            let outcome = match self.events.get(self.next) {
                Some(Entry {
                    event: Event::Host(tokens),
                    line,
                }) => step(tokens, self.host_position, self.wildcard_count, byte)
                    .map(|(position, count)| (position, count, position >= tokens.len()))
                    .map_err(|message| TransportError::Replay {
                        line: *line,
                        message,
                    }),
                Some(Entry {
                    event: Event::Disconnect,
                    ..
                }) => {
                    self.advance();
                    return Err(TransportError::Disconnected);
                }
                _ => return Err(self.unexpected("a write")),
            };
            let (position, count, complete) = outcome?;
            self.host_position = position;
            self.wildcard_count = count;
            if complete {
                self.advance();
            }
        }
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        match self.events.get(self.next).map(|entry| &entry.event) {
            Some(Event::Device(bytes)) => {
                let rest = bytes.get(self.device_offset..).unwrap_or(&[]);
                let count = rest.len().min(buffer.len());
                if let (Some(destination), Some(source)) =
                    (buffer.get_mut(..count), rest.get(..count))
                {
                    destination.copy_from_slice(source);
                }
                self.device_offset += count;
                if self.device_offset >= bytes.len() {
                    self.advance();
                }
                Ok(count)
            }
            Some(Event::Disconnect) => {
                self.advance();
                Err(TransportError::Disconnected)
            }
            Some(Event::Timeout) => {
                self.advance();
                Ok(0)
            }
            _ => Ok(0),
        }
    }

    /// Consumes the next event when `matches` accepts it, else reports a divergence.
    fn consume(
        &mut self,
        matches: impl Fn(&Event) -> bool,
        got: &str,
    ) -> Result<(), TransportError> {
        match self.events.get(self.next) {
            Some(entry) if matches(&entry.event) => {
                self.advance();
                Ok(())
            }
            _ => Err(self.unexpected(got)),
        }
    }
}

/// The port of a replayed session.
pub struct ReplayPort {
    state: Rc<RefCell<State>>,
    identity: Option<UsbIdentity>,
}

/// The provider of a replayed session.
pub struct ReplayProvider {
    state: Rc<RefCell<State>>,
}

/// Replays `transcript`: the port starts with its identity, the provider opens the ports of its
/// `! reopen` events, and both consume the same events in order.
pub fn replay(transcript: Transcript) -> (ReplayPort, ReplayProvider) {
    let state = Rc::new(RefCell::new(State {
        events: transcript.events,
        next: 0,
        host_position: 0,
        wildcard_count: 0,
        device_offset: 0,
    }));
    (
        ReplayPort {
            state: Rc::clone(&state),
            identity: transcript.identity,
        },
        ReplayProvider { state },
    )
}

impl ReplayProvider {
    /// Fails, naming the line of the first event that was never reached.
    pub fn finish(&self) -> Result<(), TransportError> {
        let state = self.state.borrow();
        match state.events.get(state.next) {
            None => Ok(()),
            Some(entry) => Err(TransportError::Replay {
                line: entry.line,
                message: format!("the session ended before {}", entry.event),
            }),
        }
    }
}

impl SerialPort for ReplayPort {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, TransportError> {
        self.state.borrow_mut().read(buffer)
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.state.borrow_mut().write(data)
    }

    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError> {
        let got = format!("! baud {baud_rate}");
        self.state
            .borrow_mut()
            .consume(|event| *event == Event::BaudRate(baud_rate), &got)
    }

    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError> {
        let got = format!(
            "! signals dtr={} rts={}",
            u8::from(signals.dtr),
            u8::from(signals.rts)
        );
        self.state
            .borrow_mut()
            .consume(|event| *event == Event::Signals(signals), &got)
    }

    async fn sleep(&mut self, duration: Duration) {
        // The trait gives a sleep no way to fail; a divergence surfaces at the next fallible
        // call or at `finish`, because the event is left unconsumed.
        let milliseconds = duration.as_millis();
        let got = format!("! sleep {milliseconds}");
        let _ = self.state.borrow_mut().consume(
            |event| matches!(event, Event::Sleep(expected) if expected.as_millis() == milliseconds),
            &got,
        );
    }

    fn identity(&self) -> Option<UsbIdentity> {
        self.identity.clone()
    }
}

impl PortProvider for ReplayProvider {
    type Port = ReplayPort;

    async fn open(
        &mut self,
        filter: &DeviceFilter,
        _baud_rate: u32,
        _timeout: Duration,
    ) -> Result<ReplayPort, TransportError> {
        let mut state = self.state.borrow_mut();
        let identity = match state.events.get(state.next).map(|entry| &entry.event) {
            // The port of the transcript must be one the filter selects.
            Some(Event::Reopen(identity))
                if identity
                    .as_ref()
                    .is_none_or(|identity| filter.matches(identity)) =>
            {
                identity.clone()
            }
            _ => return Err(state.unexpected(&format!("an open of {filter}"))),
        };
        state.advance();
        drop(state);
        Ok(ReplayPort {
            state: Rc::clone(&self.state),
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollster::block_on;

    fn port(text: &str) -> (ReplayPort, ReplayProvider) {
        replay(Transcript::parse(text).unwrap())
    }

    const TIMEOUT: Duration = Duration::from_millis(100);

    #[test]
    fn replay_follows_the_transcript() {
        let (mut port, provider) = port(
            "> c0 01 02 c0\n< c0 05 06 07 c0\n! signals dtr=1 rts=0\n! baud 460800\n! sleep 100",
        );
        block_on(async {
            port.write(&[0xc0, 0x01]).await.unwrap();
            port.write(&[0x02, 0xc0]).await.unwrap();
            let mut buffer = [0u8; 3];
            assert_eq!(port.read(&mut buffer, TIMEOUT).await.unwrap(), 3);
            assert_eq!(buffer, [0xc0, 5, 6]);
            assert_eq!(port.read(&mut buffer, TIMEOUT).await.unwrap(), 2);
            assert_eq!(buffer[..2], [7, 0xc0]);
            port.set_signals(Signals {
                dtr: true,
                rts: false,
            })
            .await
            .unwrap();
            port.set_baud_rate(460_800).await.unwrap();
            port.sleep(Duration::from_millis(100)).await;
        });
        provider.finish().unwrap();
    }

    #[test]
    fn replay_spans_lines_with_one_write() {
        let (mut port, provider) = port("> c0 01\n> 02 c0");
        block_on(port.write(&[0xc0, 1, 2, 0xc0])).unwrap();
        provider.finish().unwrap();
    }

    #[test]
    fn replay_wildcard_matches_any_payload() {
        let (mut port, provider) = port("> c0 00 11 .. c0");
        block_on(port.write(&[0xc0, 0, 0x11, 0xaa, 0xbb, 0xcc, 0xc0])).unwrap();
        provider.finish().unwrap();
        // The wildcard needs at least one byte before the closing literal.
        let (mut port, provider) = self::port("> c0 .. c0");
        block_on(port.write(&[0xc0, 0xc0])).unwrap();
        assert!(provider.finish().is_err());
    }

    #[test]
    fn replay_reports_divergence() {
        let (mut port, _) = port("> c0 01\n");
        match block_on(port.write(&[0xc0, 0x02])) {
            Err(TransportError::Replay { line, message }) => {
                assert_eq!(line, 1);
                assert!(message.contains("expected 01, got 02"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        let (mut port, _) = self::port("# first\n! signals dtr=1 rts=0");
        match block_on(port.set_signals(Signals {
            dtr: false,
            rts: false,
        })) {
            Err(TransportError::Replay { line, .. }) => assert_eq!(line, 2),
            other => panic!("{other:?}"),
        }
        let (mut port, provider) = self::port("> c0\n! baud 9600");
        block_on(port.write(&[0xc0])).unwrap();
        match provider.finish() {
            Err(TransportError::Replay { line, .. }) => assert_eq!(line, 2),
            other => panic!("{other:?}"),
        }
        match block_on(port.write(&[0xc0])) {
            Err(TransportError::Replay { line, message }) => {
                assert_eq!(line, 2);
                assert!(message.contains("got a write"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn read_before_the_device_speaks_times_out() {
        let (mut port, provider) = port("> c0\n! timeout\n< 01");
        let mut buffer = [0u8; 4];
        block_on(async {
            assert_eq!(port.read(&mut buffer, TIMEOUT).await.unwrap(), 0);
            port.write(&[0xc0]).await.unwrap();
            assert_eq!(port.read(&mut buffer, TIMEOUT).await.unwrap(), 0);
            assert_eq!(port.read(&mut buffer, TIMEOUT).await.unwrap(), 1);
        });
        provider.finish().unwrap();
        let (mut port, provider) = self::port("> c0");
        assert_eq!(block_on(port.read(&mut buffer, TIMEOUT)).unwrap(), 0);
        assert!(provider.finish().is_err());
    }

    #[test]
    fn disconnect_then_reopen() {
        let (mut port, mut provider) =
            port("! identity 303a:1001\n! disconnect\n! reopen 303a:1001 abc\n< 01");
        assert_eq!(port.identity().unwrap().product, 0x1001);
        let mut buffer = [0u8; 4];
        block_on(async {
            assert_eq!(
                port.read(&mut buffer, TIMEOUT).await,
                Err(TransportError::Disconnected)
            );
            let filter = DeviceFilter {
                vendor: 0x303a,
                product: None,
                serial_number: None,
            };
            let mut reopened = provider.open(&filter, 115_200, TIMEOUT).await.unwrap();
            let identity = reopened.identity().unwrap();
            assert_eq!((identity.vendor, identity.product), (0x303a, 0x1001));
            assert_eq!(identity.serial_number.as_deref(), Some("abc"));
            assert_eq!(reopened.read(&mut buffer, TIMEOUT).await.unwrap(), 1);
        });
        provider.finish().unwrap();
        assert!(
            block_on(provider.open(
                &DeviceFilter {
                    vendor: 1,
                    product: None,
                    serial_number: None,
                },
                9600,
                TIMEOUT
            ))
            .is_err()
        );
    }

    #[test]
    fn write_after_disconnect_fails_too() {
        let (mut port, _) = port("! disconnect");
        assert_eq!(
            block_on(port.write(&[1])),
            Err(TransportError::Disconnected)
        );
    }
}
