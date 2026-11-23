//! The DTR and RTS sequences that put an Espressif chip in download mode and back.
//!
//! On a board with a UART bridge, RTS drives EN (reset) and DTR drives GPIO0 (BOOT), both
//! active low through a pair of transistors; the USB-Serial-JTAG peripheral reads the same lines
//! as requests. The timings follow espflash 4.7.0 and esptool-js, adapted to setting both lines
//! at once.

use alloc::string::ToString;

use crate::error::Error;
use crate::manifest::{Protocol, ResetSequence};
use crate::transport::{Duration, SerialPort, Signals, TransportError};

/// One step of a sequence: set DTR and RTS, then wait this many milliseconds (none when 0).
type Step = (bool, bool, u64);

/// The USB-Serial-JTAG sequence into download mode.
const USB_SERIAL_JTAG_DOWNLOAD: [Step; 5] = [
    (false, false, 100),
    (true, false, 100),
    (true, true, 0),
    (false, true, 100),
    (false, false, 0),
];

/// The classic sequence into the application: pulse EN.
const CLASSIC_APPLICATION: [Step; 2] = [(false, true, 100), (false, false, 0)];

/// The USB-Serial-JTAG sequence into the application.
const USB_SERIAL_JTAG_APPLICATION: [Step; 3] =
    [(false, false, 100), (false, true, 100), (false, false, 0)];

/// The error for a sequence the Espressif backend cannot run.
pub(crate) fn unsupported(sequence: &ResetSequence) -> Error {
    Error::UnsupportedReset {
        protocol: Protocol::Espressif,
        reset: sequence.to_string(),
    }
}

async fn run<S: SerialPort>(port: &mut S, steps: &[Step]) -> Result<(), TransportError> {
    for &(dtr, rts, wait) in steps {
        port.set_signals(Signals { dtr, rts }).await?;
        if wait > 0 {
            port.sleep(Duration::from_millis(wait)).await;
        }
    }
    Ok(())
}

/// Resets the chip into download mode. `attempt` counts from 0: the classic sequence holds
/// GPIO0 low for 50 ms on even attempts and 500 ms on odd ones, since boards differ in how long
/// their EN capacitor takes to charge.
pub(crate) async fn enter_download_mode<S: SerialPort>(
    port: &mut S,
    sequence: &ResetSequence,
    attempt: u8,
) -> Result<(), Error> {
    match sequence {
        ResetSequence::EspressifClassic => {
            let hold = if attempt % 2 == 0 { 50 } else { 500 };
            let steps: [Step; 5] = [
                (false, false, 0),
                (true, true, 0),
                (false, true, 100),
                (true, false, hold),
                (false, false, 0),
            ];
            run(port, &steps).await?;
        }
        ResetSequence::EspressifUsbSerialJtag => run(port, &USB_SERIAL_JTAG_DOWNLOAD).await?,
        ResetSequence::None => {}
        ResetSequence::Custom(steps) => {
            for step in steps {
                port.set_signals(Signals {
                    dtr: step.dtr,
                    rts: step.rts,
                })
                .await?;
                if !step.wait.is_zero() {
                    port.sleep(step.wait).await;
                }
            }
        }
        ResetSequence::Touch1200 | ResetSequence::Uf2Touch1200 => {
            return Err(unsupported(sequence));
        }
    }
    Ok(())
}

/// Resets the chip into its application through the lines. Returns `false` when the sequence
/// has no line sequence for it (`none`, `custom`): the caller then sends the ROM loader's soft
/// reset. A port that disconnects meanwhile counts as success, since the USB device left to
/// start the application.
pub(crate) async fn reset_to_application<S: SerialPort>(
    port: &mut S,
    sequence: &ResetSequence,
) -> Result<bool, Error> {
    let steps: &[Step] = match sequence {
        ResetSequence::EspressifClassic => &CLASSIC_APPLICATION,
        ResetSequence::EspressifUsbSerialJtag => &USB_SERIAL_JTAG_APPLICATION,
        ResetSequence::None | ResetSequence::Custom(_) => return Ok(false),
        ResetSequence::Touch1200 | ResetSequence::Uf2Touch1200 => {
            return Err(unsupported(sequence));
        }
    };
    match run(port, steps).await {
        Ok(()) | Err(TransportError::Disconnected) => Ok(true),
        Err(error) => Err(error.into()),
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;
    use crate::manifest::ResetStep;
    use crate::testing::{Transcript, replay};
    use pollster::block_on;

    fn check(text: &str, action: impl FnOnce(&mut crate::testing::ReplayPort)) {
        let (mut port, provider) = replay(Transcript::parse(text).unwrap());
        action(&mut port);
        provider.finish().unwrap();
    }

    #[test]
    fn classic_into_the_application_pulses_en() {
        check(
            "! signals dtr=0 rts=1\n! sleep 100\n! signals dtr=0 rts=0",
            |port| {
                let reset = ResetSequence::EspressifClassic;
                assert_eq!(block_on(reset_to_application(port, &reset)), Ok(true));
            },
        );
    }

    #[test]
    fn usb_serial_jtag_into_the_application_survives_the_device_leaving() {
        check(
            "! signals dtr=0 rts=0\n! sleep 100\n! signals dtr=0 rts=1\n! sleep 100\n! signals dtr=0 rts=0",
            |port| {
                let reset = ResetSequence::EspressifUsbSerialJtag;
                assert_eq!(block_on(reset_to_application(port, &reset)), Ok(true));
            },
        );
        // A device that leaves as soon as EN is pulled.
        struct Leaving;
        impl SerialPort for Leaving {
            async fn read(&mut self, _: &mut [u8], _: Duration) -> Result<usize, TransportError> {
                Ok(0)
            }
            async fn write(&mut self, _: &[u8]) -> Result<(), TransportError> {
                Ok(())
            }
            async fn set_baud_rate(&mut self, _: u32) -> Result<(), TransportError> {
                Ok(())
            }
            async fn set_signals(&mut self, _: Signals) -> Result<(), TransportError> {
                Err(TransportError::Disconnected)
            }
            async fn sleep(&mut self, _: Duration) {}
            fn identity(&self) -> Option<crate::transport::UsbIdentity> {
                None
            }
        }
        let reset = ResetSequence::EspressifUsbSerialJtag;
        assert_eq!(
            block_on(reset_to_application(&mut Leaving, &reset)),
            Ok(true)
        );
        assert_eq!(
            block_on(enter_download_mode(&mut Leaving, &reset, 0)),
            Err(Error::Transport {
                error: TransportError::Disconnected,
                during: None
            })
        );
    }

    #[test]
    fn none_and_custom_leave_the_application_reset_to_the_loader() {
        check("", |port| {
            for reset in [ResetSequence::None, ResetSequence::Custom(alloc::vec![])] {
                assert_eq!(block_on(reset_to_application(port, &reset)), Ok(false));
            }
        });
    }

    #[test]
    fn custom_steps_set_the_lines_then_wait() {
        let reset = ResetSequence::Custom(alloc::vec![
            ResetStep {
                dtr: true,
                rts: false,
                wait: Duration::from_millis(20),
            },
            ResetStep {
                dtr: false,
                rts: false,
                wait: Duration::ZERO,
            },
        ]);
        check(
            "! signals dtr=1 rts=0\n! sleep 20\n! signals dtr=0 rts=0",
            |port| block_on(enter_download_mode(port, &reset, 0)).unwrap(),
        );
        check("", |port| {
            block_on(enter_download_mode(port, &ResetSequence::None, 3)).unwrap();
        });
    }

    #[test]
    fn touch_resets_do_not_apply() {
        check("", |port| {
            assert_eq!(
                block_on(enter_download_mode(port, &ResetSequence::Touch1200, 0)),
                Err(Error::UnsupportedReset {
                    protocol: Protocol::Espressif,
                    reset: "touch-1200".into()
                })
            );
        });
    }
}
