//! Ports that record what passes through them into a transcript.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::transcript::{Entry, Event, HostToken, Transcript};
use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};

struct Recording {
    identity: Option<UsbIdentity>,
    events: Vec<Event>,
}

/// A port that records every call into the transcript it shares with its provider.
pub struct RecordingPort<S> {
    inner: S,
    recording: Rc<RefCell<Recording>>,
}

/// A provider that records its opens, and wraps the ports it opens in recording ports.
pub struct RecordingProvider<P> {
    inner: P,
    recording: Rc<RefCell<Recording>>,
}

/// Records the session of `port` and of the ports `provider` opens.
pub fn record<P: PortProvider>(
    port: P::Port,
    provider: P,
) -> (RecordingPort<P::Port>, RecordingProvider<P>) {
    let recording = Rc::new(RefCell::new(Recording {
        identity: port.identity(),
        events: Vec::new(),
    }));
    (
        RecordingPort {
            inner: port,
            recording: Rc::clone(&recording),
        },
        RecordingProvider {
            inner: provider,
            recording,
        },
    )
}

impl<P: PortProvider> RecordingProvider<P> {
    /// The session recorded so far.
    pub fn transcript(&self) -> Transcript {
        let recording = self.recording.borrow();
        let first = usize::from(recording.identity.is_some()) + 1;
        Transcript {
            identity: recording.identity.clone(),
            events: recording
                .events
                .iter()
                .enumerate()
                .map(|(index, event)| Entry {
                    line: first + index,
                    event: event.clone(),
                })
                .collect(),
        }
    }

    /// The provider being recorded.
    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl Recording {
    fn note_error(&mut self, error: &TransportError) {
        if *error == TransportError::Disconnected {
            self.events.push(Event::Disconnect);
        }
    }
}

impl<S: SerialPort> SerialPort for RecordingPort<S> {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        let result = self.inner.read(buffer, timeout).await;
        let mut recording = self.recording.borrow_mut();
        match &result {
            Ok(0) => recording.events.push(Event::Timeout),
            Ok(count) => {
                let received = buffer.get(..*count).unwrap_or(&[]);
                recording.events.push(Event::Device(received.to_vec()));
            }
            Err(error) => recording.note_error(error),
        }
        result
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let result = self.inner.write(data).await;
        let mut recording = self.recording.borrow_mut();
        match &result {
            Ok(()) if !data.is_empty() => recording.events.push(Event::Host(
                data.iter().map(|byte| HostToken::Byte(*byte)).collect(),
            )),
            Ok(()) => {}
            Err(error) => recording.note_error(error),
        }
        result
    }

    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError> {
        let result = self.inner.set_baud_rate(baud_rate).await;
        let mut recording = self.recording.borrow_mut();
        match &result {
            Ok(()) => recording.events.push(Event::BaudRate(baud_rate)),
            Err(error) => recording.note_error(error),
        }
        result
    }

    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError> {
        let result = self.inner.set_signals(signals).await;
        let mut recording = self.recording.borrow_mut();
        match &result {
            Ok(()) => recording.events.push(Event::Signals(signals)),
            Err(error) => recording.note_error(error),
        }
        result
    }

    async fn sleep(&mut self, duration: Duration) {
        self.inner.sleep(duration).await;
        self.recording
            .borrow_mut()
            .events
            .push(Event::Sleep(duration));
    }

    fn identity(&self) -> Option<UsbIdentity> {
        self.inner.identity()
    }
}

impl<P: PortProvider> PortProvider for RecordingProvider<P> {
    type Port = RecordingPort<P::Port>;

    async fn open(
        &mut self,
        filter: &DeviceFilter,
        baud_rate: u32,
        timeout: Duration,
    ) -> Result<Self::Port, TransportError> {
        let port = self.inner.open(filter, baud_rate, timeout).await?;
        self.recording
            .borrow_mut()
            .events
            .push(Event::Reopen(port.identity()));
        Ok(RecordingPort {
            inner: port,
            recording: Rc::clone(&self.recording),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::replay;
    use pollster::block_on;

    #[test]
    fn recording_a_replay_gives_the_same_transcript() {
        let transcript = Transcript::parse(
            "! identity 303a:1001\n\
             > c0 01 02 c0\n\
             < c0 05 06 07 c0\n\
             ! signals dtr=1 rts=0\n\
             ! baud 460800\n\
             ! sleep 100\n\
             ! timeout\n\
             ! disconnect\n\
             ! reopen 303a:1001 abc\n\
             < 01 02\n",
        )
        .unwrap();
        let (port, provider) = replay(transcript.clone());
        let (mut port, mut provider) = record(port, provider);
        let timeout = Duration::from_millis(10);
        let mut buffer = [0u8; 16];
        block_on(async {
            port.write(&[0xc0, 1, 2, 0xc0]).await.unwrap();
            assert_eq!(port.read(&mut buffer, timeout).await.unwrap(), 5);
            port.set_signals(Signals {
                dtr: true,
                rts: false,
            })
            .await
            .unwrap();
            port.set_baud_rate(460_800).await.unwrap();
            port.sleep(Duration::from_millis(100)).await;
            assert_eq!(port.read(&mut buffer, timeout).await.unwrap(), 0);
            assert_eq!(
                port.read(&mut buffer, timeout).await,
                Err(TransportError::Disconnected)
            );
            let filter = DeviceFilter {
                vendor: 0x303a,
                product: None,
            };
            let mut reopened = provider.open(&filter, 115_200, timeout).await.unwrap();
            assert_eq!(reopened.read(&mut buffer, timeout).await.unwrap(), 2);
        });
        assert_eq!(provider.transcript(), transcript);
        provider.inner().finish().unwrap();
    }
}
