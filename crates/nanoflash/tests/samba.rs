//! Transcript tests of the SAM-BA backend.

use std::fmt::Write as _;
use std::path::Path;

use nanoflash::backend::{ChipInformation, ProgressEvent};
use nanoflash::manifest::{Protocol, ResetSequence};
use nanoflash::samba::{SambaBackend, SambaOptions};
use nanoflash::testing::{ReplayPort, ReplayProvider, Transcript, replay};
use nanoflash::transport::{
    DeviceFilter, Duration, PortProvider, SerialPort, Signals, TransportError, UsbIdentity,
};
use nanoflash::{Error, ErrorClass};
use pollster::block_on;

fn transcript(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/transcripts/samba")
        .join(format!("{name}.transcript"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

struct Messages<'a>(&'a mut Vec<String>);

impl nanoflash::backend::Progress for Messages<'_> {
    fn report(&mut self, _event: ProgressEvent) {}

    fn message(&mut self, text: &str) {
        self.0.push(text.to_string());
    }
}

/// What a connection over a replayed transcript gave.
struct Connection {
    result: Result<ChipInformation, Error>,
    provider: ReplayProvider,
    messages: Vec<String>,
    chip: Option<&'static str>,
}

/// Connects over a replay of `text`.
fn connect_text(text: &str, reset: Option<ResetSequence>, options: SambaOptions) -> Connection {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let mut messages = Vec::new();
    let (result, chip) = {
        let mut backend = match SambaBackend::new(port, &mut provider, reset, options) {
            Ok(backend) => backend,
            Err(error) => panic!("{error}"),
        };
        let result = block_on(backend.connect(&mut Messages(&mut messages)));
        (result, backend.chip())
    };
    Connection {
        result,
        provider,
        messages,
        chip,
    }
}

/// Connects over a replay of `tests/transcripts/samba/<name>.transcript`.
fn connect(
    name: &str,
    reset: Option<ResetSequence>,
    options: SambaOptions,
) -> (Result<ChipInformation, Error>, ReplayProvider) {
    let connection = connect_text(&transcript(name), reset, options);
    (connection.result, connection.provider)
}

fn identity(vendor: u16, product: u16) -> Option<UsbIdentity> {
    Some(UsbIdentity {
        vendor,
        product,
        serial_number: None,
    })
}

/// The options of a manifest that lists the Nano 33 BLE's bootloader.
fn nano33ble() -> SambaOptions {
    SambaOptions {
        bootloader_devices: vec![DeviceFilter {
            vendor: 0x2341,
            product: Some(0x005a),
            serial_number: None,
        }],
        baud_rate: None,
    }
}

#[test]
fn connects_to_a_nano_33_ble_after_the_touch() {
    let connection = connect_text(
        &transcript("connect-nano33ble"),
        Some(ResetSequence::Touch1200),
        nano33ble(),
    );
    assert_eq!(
        connection.result.unwrap(),
        ChipInformation {
            protocol: Protocol::Samba,
            chip: "nrf52840".into(),
            flash_size: Some(0x10_0000),
            secure_download_mode: false,
            usb: identity(0x2341, 0x005a),
            writable_from: Some(0x1_0000),
        }
    );
    assert_eq!(connection.chip, Some("nrf52840"));
    assert!(connection.messages.is_empty(), "{:?}", connection.messages);
    connection.provider.finish().unwrap();
}

#[test]
fn the_bootloader_is_found_without_a_manifest() {
    // The built-in rule: an Arduino sketch's product 0x805a has its bootloader on 0x005a.
    let (result, provider) = connect("connect-nano33ble", None, SambaOptions::default());
    assert_eq!(result.unwrap().usb, identity(0x2341, 0x005a));
    provider.finish().unwrap();
}

#[test]
fn bootloader_port_is_used_as_is() {
    let (result, provider) = connect(
        "connect-nano33ble-bootloader",
        Some(ResetSequence::Touch1200),
        nano33ble(),
    );
    let information = result.unwrap();
    assert_eq!(information.chip, "nrf52840");
    assert_eq!(information.usb, identity(0x2341, 0x005a));
    provider.finish().unwrap();
}

#[test]
fn port_without_identity_needs_no_touch() {
    let (result, provider) = connect("connect-emulator", None, SambaOptions::default());
    let information = result.unwrap();
    assert_eq!(information.chip, "nrf52840");
    assert_eq!(information.usb, None);
    provider.finish().unwrap();
}

#[test]
fn reset_none_needs_no_touch() {
    // `none` trusts the board to sit in its bootloader, whatever its identity says.
    let text = transcript("connect-nano33ble-bootloader").replace("2341:005a", "2341:805a");
    let connection = connect_text(&text, Some(ResetSequence::None), SambaOptions::default());
    assert_eq!(connection.result.unwrap().chip, "nrf52840");
    connection.provider.finish().unwrap();
}

#[test]
fn custom_resets_run_their_steps_before_reopening() {
    let reset = ResetSequence::Custom(vec![
        nanoflash::manifest::ResetStep {
            dtr: true,
            rts: false,
            wait: Duration::from_millis(20),
        },
        nanoflash::manifest::ResetStep {
            dtr: false,
            rts: false,
            wait: Duration::ZERO,
        },
    ]);
    let text = transcript("connect-nano33ble").replace(
        "! baud 1200\n! signals dtr=0 rts=0\n",
        "! signals dtr=1 rts=0\n! sleep 20\n! signals dtr=0 rts=0\n",
    );
    let connection = connect_text(&text, Some(reset), nano33ble());
    assert_eq!(connection.result.unwrap().chip, "nrf52840");
    connection.provider.finish().unwrap();
}

/// A port whose device leaves as soon as the lines change, as a sketch may on the touch.
enum Port {
    Leaving,
    Replay(ReplayPort),
}

/// Opens the ports of a replayed transcript.
struct Provider(ReplayProvider);

impl SerialPort for Port {
    async fn read(
        &mut self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransportError> {
        match self {
            Port::Leaving => Err(TransportError::Disconnected),
            Port::Replay(port) => port.read(buffer, timeout).await,
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            Port::Leaving => Err(TransportError::Disconnected),
            Port::Replay(port) => port.write(data).await,
        }
    }

    async fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), TransportError> {
        match self {
            Port::Leaving => Ok(()),
            Port::Replay(port) => port.set_baud_rate(baud_rate).await,
        }
    }

    async fn set_signals(&mut self, signals: Signals) -> Result<(), TransportError> {
        match self {
            Port::Leaving => Err(TransportError::Disconnected),
            Port::Replay(port) => port.set_signals(signals).await,
        }
    }

    async fn sleep(&mut self, duration: Duration) {
        if let Port::Replay(port) = self {
            port.sleep(duration).await;
        }
    }

    fn identity(&self) -> Option<UsbIdentity> {
        match self {
            Port::Leaving => identity(0x2341, 0x805a),
            Port::Replay(port) => port.identity(),
        }
    }
}

impl PortProvider for Provider {
    type Port = Port;

    async fn open(
        &mut self,
        filter: &DeviceFilter,
        baud_rate: u32,
        timeout: Duration,
    ) -> Result<Port, TransportError> {
        self.0
            .open(filter, baud_rate, timeout)
            .await
            .map(Port::Replay)
    }
}

#[test]
fn a_sketch_that_leaves_during_the_touch_is_reopened() {
    let text = transcript("connect-nano33ble-bootloader")
        .replace("! identity 2341:005a\n", "! reopen 2341:005a\n");
    let (_, provider) = replay(Transcript::parse(&text).unwrap());
    let mut provider = Provider(provider);
    let result = {
        let mut backend = match SambaBackend::new(Port::Leaving, &mut provider, None, nano33ble()) {
            Ok(backend) => backend,
            Err(error) => panic!("{error}"),
        };
        block_on(backend.connect(&mut ()))
    };
    assert_eq!(result.unwrap().usb, identity(0x2341, 0x005a));
    provider.0.finish().unwrap();
}

#[test]
fn connects_to_a_samd21_bootloader() {
    let connection = connect_text(
        &transcript("connect-mkrwifi1010"),
        None,
        SambaOptions::default(),
    );
    assert_eq!(
        connection.result.unwrap(),
        ChipInformation {
            protocol: Protocol::Samba,
            chip: "samd21g18a".into(),
            flash_size: Some(0x4_0000),
            secure_download_mode: false,
            usb: identity(0x2341, 0x0054),
            writable_from: Some(0x2000),
        }
    );
    assert_eq!(connection.chip, Some("samd21g18a"));
    connection.provider.finish().unwrap();
}

#[test]
fn the_filter_of_the_matching_product_is_chosen() {
    // The manifest lists two bootloaders: the one of the sketch's product is waited for.
    let options = SambaOptions {
        bootloader_devices: vec![
            DeviceFilter {
                vendor: 0x2341,
                product: Some(0x005a),
                serial_number: None,
            },
            DeviceFilter {
                vendor: 0x2341,
                product: Some(0x0054),
                serial_number: None,
            },
        ],
        baud_rate: None,
    };
    let (result, provider) = connect("connect-mkrwifi1010", None, options);
    assert_eq!(result.unwrap().usb, identity(0x2341, 0x0054));
    provider.finish().unwrap();
}

#[test]
fn no_bootloader_device_after_the_touch() {
    let (result, provider) = connect("connect-no-bootloader-device", None, nano33ble());
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::NoBootloader {
            waited_for: Some("2341:005a".into())
        }
    );
    assert_eq!(error.class(), ErrorClass::Device);
    assert!(error.to_string().contains("double-tap"), "{error}");
    provider.finish().unwrap();
}

#[test]
fn browser_permission_is_kept() {
    let (result, provider) = connect("connect-not-granted", None, nano33ble());
    match result.unwrap_err() {
        Error::Transport {
            error: TransportError::NotGranted { filter },
            ..
        } => assert_eq!(filter, "2341:005a"),
        other => panic!("{other:?}"),
    }
    provider.finish().unwrap();
}

#[test]
fn silent_port_gives_no_bootloader() {
    let (result, provider) = connect("connect-silent", None, nano33ble());
    assert_eq!(
        result.unwrap_err(),
        Error::NoBootloader { waited_for: None }
    );
    provider.finish().unwrap();
}

#[test]
fn endless_text_gives_no_bootloader() {
    // The first V is answered by 1024 bytes without a line end: the reply stops at 256 bytes,
    // the next attempt's drain takes the other 768, and the last two attempts get no answer.
    let mut text = String::from("! identity 2341:005a\n> \"N#\"\n< \"\\n\\r\"\n> \"V#\"\n<");
    for _ in 0..1024 {
        text.push_str(" 41");
    }
    text.push('\n');
    for _ in 0..2 {
        writeln!(text, "> \"N#\"\n> \"V#\"").unwrap();
    }
    let connection = connect_text(&text, None, nano33ble());
    assert_eq!(
        connection.result.unwrap_err(),
        Error::NoBootloader { waited_for: None }
    );
    connection.provider.finish().unwrap();
}

#[test]
fn version_lines_stop_at_256_bytes() {
    // A 300-byte line: its end lies past the budget, so it is no version line; the next drain
    // takes the rest.
    let mut text = String::from("! identity 2341:005a\n> \"N#\"\n< \"\\n\\r\"\n> \"V#\"\n<");
    for _ in 0..300 {
        text.push_str(" 41");
    }
    text.push_str(" 0a 0d\n");
    for _ in 0..2 {
        writeln!(text, "> \"N#\"\n> \"V#\"").unwrap();
    }
    let connection = connect_text(&text, None, nano33ble());
    assert_eq!(
        connection.result.unwrap_err(),
        Error::NoBootloader { waited_for: None }
    );
    connection.provider.finish().unwrap();
}

#[test]
fn the_drain_stops_after_4096_bytes() {
    // The port sends 5000 bytes before any command: the drain gives up after 4096 and sends
    // N# while the device is still talking, which the replay reports.
    let mut text = String::from("! identity 2341:005a\n<");
    for _ in 0..5000 {
        text.push_str(" 41");
    }
    text.push_str("\n> \"N#\"\n");
    let connection = connect_text(&text, None, nano33ble());
    match connection.result.unwrap_err() {
        Error::Transport {
            error: TransportError::Replay { line: 2, message },
            ..
        } => assert!(message.contains("got a write"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn plain_sam_ba_is_refused() {
    let (result, provider) = connect("connect-plain-samba", None, nano33ble());
    assert_eq!(
        result.unwrap_err(),
        Error::UnsupportedBootloader {
            version: "v1.1 Dec 15 2010".into()
        }
    );
    provider.finish().unwrap();
}

#[test]
fn a_missing_extension_is_refused() {
    // Without Z, nothing could be verified.
    let text = transcript("connect-nano33ble-bootloader")
        .replace("[Arduino:IKXYZ]", "[Arduino:IKXY]")
        .replace("> \"I#\"\n< \"nRF52840-QIAA\\n\\r\"\n", "");
    let connection = connect_text(&text, None, nano33ble());
    assert_eq!(
        connection.result.unwrap_err(),
        Error::UnsupportedBootloader {
            version: "Arduino Bootloader (SAM-BA extended) 2.0 [Arduino:IKXY]".into()
        }
    );
    connection.provider.finish().unwrap();
}

#[test]
fn unknown_chips_are_named() {
    let (result, provider) = connect("connect-unknown-chip", None, nano33ble());
    assert_eq!(
        result.unwrap_err(),
        Error::UnsupportedChip {
            chip: "R7FA4M1AB".into()
        }
    );
    provider.finish().unwrap();
}

#[test]
fn unknown_samd_identifiers_are_named() {
    let text = transcript("connect-mkrwifi1010").replace("< 05 03 01 10", "< 02 00 01 10");
    let connection = connect_text(&text, None, SambaOptions::default());
    assert_eq!(
        connection.result.unwrap_err(),
        Error::UnknownChip {
            detail: "device identifier 0x10010002".into()
        }
    );
    connection.provider.finish().unwrap();
}

#[test]
fn unanswered_identification_names_the_command() {
    let text =
        transcript("connect-nano33ble-bootloader").replace("< \"nRF52840-QIAA\\n\\r\"\n", "");
    let connection = connect_text(&text, None, nano33ble());
    assert_eq!(
        connection.result.unwrap_err(),
        Error::NoResponse { command: "I" }
    );
    connection.provider.finish().unwrap();
    // Three bytes of the four of the device identifier.
    let text = transcript("connect-mkrwifi1010").replace("< 05 03 01 10", "< 05 03 01");
    let connection = connect_text(&text, None, SambaOptions::default());
    assert_eq!(
        connection.result.unwrap_err(),
        Error::NoResponse { command: "w" }
    );
    connection.provider.finish().unwrap();
}

#[test]
fn unknown_bootloader_identity() {
    let connection = connect_text(
        "! identity 1209:0001",
        Some(ResetSequence::Touch1200),
        SambaOptions::default(),
    );
    let error = connection.result.unwrap_err();
    assert_eq!(
        error,
        Error::UnknownBootloaderDevice {
            identity: identity(0x1209, 0x0001).unwrap()
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    connection.provider.finish().unwrap();
}

#[test]
fn espressif_resets_are_refused() {
    for (reset, name) in [
        (ResetSequence::EspressifClassic, "espressif-classic"),
        (
            ResetSequence::EspressifUsbSerialJtag,
            "espressif-usb-serial-jtag",
        ),
        (ResetSequence::Uf2Touch1200, "uf2-touch-1200"),
    ] {
        let (port, mut provider) = replay(Transcript::parse("").unwrap());
        match SambaBackend::new(port, &mut provider, Some(reset), SambaOptions::default()) {
            Err(error) => assert_eq!(
                error,
                Error::UnsupportedReset {
                    protocol: Protocol::Samba,
                    reset: name.into()
                }
            ),
            Ok(_) => panic!("{name} was accepted"),
        }
    }
}

#[test]
fn baud_rate_is_reported_as_ignored() {
    let options = SambaOptions {
        baud_rate: Some(921_600),
        ..nano33ble()
    };
    let connection = connect_text(&transcript("connect-nano33ble-bootloader"), None, options);
    connection.result.unwrap();
    assert_eq!(connection.messages.len(), 1, "{:?}", connection.messages);
    assert!(
        connection.messages[0].contains("ignored"),
        "{:?}",
        connection.messages
    );
    connection.provider.finish().unwrap();
}
