//! The 1200-baud touch shared by the bootloaders reached over the USB port of a sketch (SAM-BA
//! and AVR109): opening the port at 1200 baud with DTR cleared asks the sketch to restart into
//! its bootloader, which shows another USB identity; the backend then waits for that device.

// The AVR backend comes with its module; until then only SAM-BA calls the touch.
#![cfg_attr(
    not(feature = "samba"),
    expect(
        dead_code,
        reason = "only the SAM-BA backend touches the sketch so far"
    )
)]

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::error::Error;
use crate::manifest::{Protocol, ResetSequence};
use crate::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};

/// The USB vendor identifier of Arduino.
const ARDUINO_VENDOR: u16 = 0x2341;
/// The USB vendor identifier of Arduino SRL (arduino.org), whose Leonardo and Micro run Caterina.
const ARDUINO_SRL_VENDOR: u16 = 0x2A03;
/// The USB vendor identifier of SparkFun.
const SPARKFUN_VENDOR: u16 = 0x1B4F;
/// The products of SparkFun's Caterina boards, as (sketch, bootloader): the Pro Micro at 5 V and
/// 16 MHz, then at 3.3 V and 8 MHz. They do not follow Arduino's bit 15.
const SPARKFUN_PRODUCTS: [(u16, u16); 2] = [(0x9206, 0x9205), (0x9204, 0x9203)];
/// Arduino's product identifiers set this bit while a sketch runs, and clear it in the
/// bootloader (`805a` for the Nano 33 BLE's sketch, `005a` for its bootloader).
const SKETCH_PRODUCT_BIT: u16 = 0x8000;
/// The baud rate the sketch reads as a request to start the bootloader.
const TOUCH_BAUD_RATE: u32 = 1200;
/// How long the bootloader's USB device may take to appear after the touch.
pub(crate) const REOPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// The vendors whose products clear bit 15 in the bootloader, for `protocol`: SAM-BA keeps to
/// Arduino's, AVR109 adds Arduino SRL's.
fn arduino_vendor(vendor: u16, protocol: Protocol) -> bool {
    vendor == ARDUINO_VENDOR || (protocol == Protocol::Avr109 && vendor == ARDUINO_SRL_VENDOR)
}

/// The product of the bootloader of the sketch `identity` by the built-in rules, when they know
/// it.
fn built_in_bootloader(identity: &UsbIdentity, protocol: Protocol) -> Option<u16> {
    if arduino_vendor(identity.vendor, protocol) {
        return Some(identity.product & !SKETCH_PRODUCT_BIT);
    }
    if protocol == Protocol::Avr109 && identity.vendor == SPARKFUN_VENDOR {
        return SPARKFUN_PRODUCTS
            .iter()
            .find(|(sketch, _)| *sketch == identity.product)
            .map(|&(_, bootloader)| bootloader);
    }
    None
}

/// The filter of the bootloader device to wait for after touching the sketch of `identity`: a
/// listed filter of its vendor and product with bit 15 cleared, else the first listed filter,
/// else the built-in rules of `protocol`.
pub(crate) fn bootloader_filter(
    identity: &UsbIdentity,
    devices: &[DeviceFilter],
    protocol: Protocol,
) -> Result<DeviceFilter, Error> {
    let product = identity.product & !SKETCH_PRODUCT_BIT;
    let same_product = devices
        .iter()
        .find(|filter| filter.vendor == identity.vendor && filter.product == Some(product));
    if let Some(filter) = same_product.or(devices.first()) {
        return Ok(filter.clone());
    }
    match built_in_bootloader(identity, protocol) {
        Some(product) => Ok(DeviceFilter {
            vendor: identity.vendor,
            product: Some(product),
            serial_number: None,
        }),
        None => Err(Error::UnknownBootloaderDevice {
            identity: identity.clone(),
        }),
    }
}

/// Whether the port of `identity` is the bootloader's already: `none` reset, no USB identity (an
/// emulator), a listed bootloader identity with a product (a vendor alone also matches the
/// sketch), an Arduino product without bit 15, or, for AVR109, a SparkFun bootloader product.
pub(crate) fn in_bootloader(
    identity: Option<&UsbIdentity>,
    reset: &ResetSequence,
    devices: &[DeviceFilter],
    protocol: Protocol,
) -> bool {
    let Some(identity) = identity else {
        return true;
    };
    *reset == ResetSequence::None
        || devices
            .iter()
            .any(|filter| filter.product.is_some() && filter.matches(identity))
        || (arduino_vendor(identity.vendor, protocol) && identity.product & SKETCH_PRODUCT_BIT == 0)
        || (protocol == Protocol::Avr109
            && identity.vendor == SPARKFUN_VENDOR
            && SPARKFUN_PRODUCTS
                .iter()
                .any(|&(_, bootloader)| bootloader == identity.product))
}

/// Touches the sketch on `port` and opens the bootloader's port at `baud_rate` in its place,
/// unless [`in_bootloader`] holds. The port stays in place on an error before the touch, and is
/// `None` once it was closed for a bootloader that did not appear.
pub(crate) async fn enter_bootloader<P: PortProvider>(
    port: &mut Option<P::Port>,
    provider: &mut P,
    reset: &ResetSequence,
    devices: &[DeviceFilter],
    protocol: Protocol,
    baud_rate: u32,
) -> Result<(), Error> {
    let Some(current) = port.as_mut() else {
        return Err(Error::from(TransportError::Disconnected));
    };
    let Some(identity) = current.identity() else {
        return Ok(());
    };
    if in_bootloader(Some(&identity), reset, devices, protocol) {
        return Ok(());
    }
    let filter = bootloader_filter(&identity, devices, protocol)?;
    match touch(current, reset, protocol).await {
        // The sketch already left for the bootloader.
        Ok(())
        | Err(Error::Transport {
            error: TransportError::Disconnected,
            ..
        }) => {}
        Err(error) => return Err(error),
    }
    // The port must be closed before the bootloader's device can be opened.
    drop(port.take());
    match provider.open(&filter, baud_rate, REOPEN_TIMEOUT).await {
        Ok(opened) => {
            *port = Some(opened);
            Ok(())
        }
        Err(TransportError::NotFound { .. }) => Err(Error::NoBootloader {
            waited_for: Some(filter.to_string()),
            reset: Some(reset.name()),
            protocol,
            rates: Vec::new(),
        }),
        Err(error) => Err(error.into()),
    }
}

/// Asks the sketch to start the bootloader: 1200 baud with DTR cleared, or the custom steps.
async fn touch<S: SerialPort>(
    port: &mut S,
    reset: &ResetSequence,
    protocol: Protocol,
) -> Result<(), Error> {
    match reset {
        ResetSequence::Touch1200 => {
            port.set_baud_rate(TOUCH_BAUD_RATE).await?;
            port.set_signals(Signals {
                dtr: false,
                rts: false,
            })
            .await?;
        }
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
        // The board sits in its bootloader already.
        ResetSequence::None => {}
        // Refused by the backends' constructors.
        sequence @ (ResetSequence::Uf2Touch1200
        | ResetSequence::EspressifClassic
        | ResetSequence::EspressifUsbSerialJtag) => {
            return Err(Error::UnsupportedReset {
                protocol,
                reset: sequence.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(vendor: u16, product: u16) -> UsbIdentity {
        UsbIdentity {
            vendor,
            product,
            serial_number: None,
        }
    }

    fn filter(vendor: u16, product: u16) -> DeviceFilter {
        DeviceFilter {
            vendor,
            product: Some(product),
            serial_number: None,
        }
    }

    #[test]
    fn bootloader_filters() {
        for (sketch, bootloader) in [
            ((0x2341, 0x8036), (0x2341, 0x0036)),
            ((0x2a03, 0x8037), (0x2a03, 0x0037)),
            ((0x1b4f, 0x9206), (0x1b4f, 0x9205)),
            ((0x1b4f, 0x9204), (0x1b4f, 0x9203)),
        ] {
            assert_eq!(
                bootloader_filter(&usb(sketch.0, sketch.1), &[], Protocol::Avr109),
                Ok(filter(bootloader.0, bootloader.1)),
                "{sketch:x?}"
            );
        }
        let listed = [filter(0x2341, 0x005a)];
        assert_eq!(
            bootloader_filter(&usb(0x1209, 0x0001), &listed, Protocol::Avr109),
            Ok(filter(0x2341, 0x005a))
        );
        // A listed filter of the sketch's vendor and product wins over the first one.
        let two = [filter(0x2341, 0x005a), filter(0x2341, 0x0036)];
        assert_eq!(
            bootloader_filter(&usb(0x2341, 0x8036), &two, Protocol::Avr109),
            Ok(filter(0x2341, 0x0036))
        );
        assert_eq!(
            bootloader_filter(&usb(0x1209, 0x0001), &[], Protocol::Avr109),
            Err(Error::UnknownBootloaderDevice {
                identity: usb(0x1209, 0x0001)
            })
        );
        // SAM-BA keeps its rule: Arduino's vendor only.
        assert_eq!(
            bootloader_filter(&usb(0x2341, 0x805a), &[], Protocol::Samba),
            Ok(filter(0x2341, 0x005a))
        );
        for (vendor, product) in [(0x2a03, 0x8037), (0x1b4f, 0x9206)] {
            assert_eq!(
                bootloader_filter(&usb(vendor, product), &[], Protocol::Samba),
                Err(Error::UnknownBootloaderDevice {
                    identity: usb(vendor, product)
                })
            );
        }
    }

    #[test]
    fn bootloader_identities_need_no_touch() {
        let touch = ResetSequence::Touch1200;
        for identity in [
            usb(0x2341, 0x0036),
            usb(0x2a03, 0x0036),
            usb(0x1b4f, 0x9205),
            usb(0x1b4f, 0x9203),
        ] {
            assert!(
                in_bootloader(Some(&identity), &touch, &[], Protocol::Avr109),
                "{identity:?}"
            );
        }
        assert!(in_bootloader(None, &touch, &[], Protocol::Avr109));
        let sketch = usb(0x2341, 0x8036);
        assert!(in_bootloader(
            Some(&sketch),
            &ResetSequence::None,
            &[],
            Protocol::Avr109
        ));
        assert!(!in_bootloader(Some(&sketch), &touch, &[], Protocol::Avr109));
        assert!(!in_bootloader(
            Some(&usb(0x1b4f, 0x9206)),
            &touch,
            &[],
            Protocol::Avr109
        ));
        // A listed identity with a product is a bootloader; a vendor alone matches the sketch.
        let listed = [filter(0x1209, 0x0002)];
        assert!(in_bootloader(
            Some(&usb(0x1209, 0x0002)),
            &touch,
            &listed,
            Protocol::Avr109
        ));
        let vendor = [DeviceFilter {
            vendor: 0x1209,
            product: None,
            serial_number: None,
        }];
        assert!(!in_bootloader(
            Some(&usb(0x1209, 0x0002)),
            &touch,
            &vendor,
            Protocol::Avr109
        ));
        // SAM-BA keeps its rule: Arduino's vendor only.
        assert!(in_bootloader(
            Some(&usb(0x2341, 0x005a)),
            &touch,
            &[],
            Protocol::Samba
        ));
        for identity in [usb(0x2a03, 0x0036), usb(0x1b4f, 0x9205)] {
            assert!(
                !in_bootloader(Some(&identity), &touch, &[], Protocol::Samba),
                "{identity:?}"
            );
        }
    }

    #[cfg(feature = "testing")]
    mod replayed {
        use super::*;
        use crate::testing::{ReplayProvider, Transcript, replay};
        use alloc::string::ToString;
        use alloc::vec;
        use pollster::block_on;

        fn enter(
            text: &str,
            reset: &ResetSequence,
        ) -> (
            Result<(), Error>,
            Option<crate::testing::ReplayPort>,
            ReplayProvider,
        ) {
            let (port, mut provider) = replay(Transcript::parse(text).unwrap());
            let mut port = Some(port);
            let result = block_on(enter_bootloader(
                &mut port,
                &mut provider,
                reset,
                &[],
                Protocol::Avr109,
                57_600,
            ));
            (result, port, provider)
        }

        #[test]
        fn touches_and_reopens() {
            let (result, port, provider) = enter(
                "! identity 2341:8036\n! baud 1200\n! signals dtr=0 rts=0\n! reopen 2341:0036",
                &ResetSequence::Touch1200,
            );
            assert_eq!(result, Ok(()));
            assert_eq!(
                port.and_then(|port| port.identity()),
                Some(usb(0x2341, 0x0036))
            );
            assert_eq!(provider.finish(), Ok(()));
        }

        #[test]
        fn a_bootloader_port_is_kept() {
            let (result, port, provider) = enter("! identity 2341:0036", &ResetSequence::Touch1200);
            assert_eq!(result, Ok(()));
            assert!(port.is_some());
            assert_eq!(provider.finish(), Ok(()));
        }

        #[test]
        fn a_missing_bootloader_names_the_protocol() {
            let (result, port, provider) = enter(
                "! identity 2341:8036\n! baud 1200\n! signals dtr=0 rts=0\n! open-fails not-found",
                &ResetSequence::Touch1200,
            );
            assert_eq!(
                result,
                Err(Error::NoBootloader {
                    waited_for: Some("2341:0036".to_string()),
                    reset: Some("touch-1200"),
                    protocol: Protocol::Avr109,
                    rates: vec![],
                })
            );
            assert!(port.is_none());
            assert_eq!(provider.finish(), Ok(()));
        }

        #[test]
        fn an_unknown_sketch_keeps_its_port() {
            let (result, port, provider) = enter("! identity 1209:0001", &ResetSequence::Touch1200);
            assert_eq!(
                result,
                Err(Error::UnknownBootloaderDevice {
                    identity: usb(0x1209, 0x0001)
                })
            );
            assert!(port.is_some());
            assert_eq!(provider.finish(), Ok(()));
        }

        #[test]
        fn custom_steps_replace_the_touch() {
            let steps = ResetSequence::Custom(vec![crate::manifest::ResetStep {
                dtr: true,
                rts: false,
                wait: crate::transport::Duration::from_millis(50),
            }]);
            let (result, _, provider) = enter(
                "! identity 2341:8036\n! signals dtr=1 rts=0\n! sleep 50\n! reopen 2341:0036",
                &steps,
            );
            assert_eq!(result, Ok(()));
            assert_eq!(provider.finish(), Ok(()));
        }
    }
}
