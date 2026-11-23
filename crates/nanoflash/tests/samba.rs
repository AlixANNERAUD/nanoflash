//! Transcript tests of the SAM-BA backend.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use nanoflash::backend::{Backend, ChipInformation, ProgressEvent, Region, Segment, Stage};
use nanoflash::flash::{FlashOptions, FlashReport, Hooks, Warning, detect, flash};
use nanoflash::manifest::{Manifest, Protocol, ResetSequence};
use nanoflash::samba::{SambaBackend, SambaOptions, crc16};
use nanoflash::testing::{
    Event, HostToken, MemorySource, ReplayPort, ReplayProvider, Transcript, record, replay,
};
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
            erases_to_end: false,
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

/// Opens the ports of a replayed transcript, keeping the baud rate and timeout of each open.
struct Provider(ReplayProvider, Vec<(u32, Duration)>);

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
        self.1.push((baud_rate, timeout));
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
    let mut provider = Provider(provider, Vec::new());
    let result = {
        let mut backend = match SambaBackend::new(Port::Leaving, &mut provider, None, nano33ble()) {
            Ok(backend) => backend,
            Err(error) => panic!("{error}"),
        };
        block_on(backend.connect(&mut ()))
    };
    assert_eq!(result.unwrap().usb, identity(0x2341, 0x005a));
    // The bootloader's port is opened at 115200 baud, and may take 15 s to appear.
    assert_eq!(provider.1, [(115_200, Duration::from_secs(15))]);
    provider.0.finish().unwrap();
}

#[test]
fn a_vendor_only_filter_keeps_the_touch() {
    // `{ vendor = 0x2341 }` also matches the sketch: only filters with a product tell a
    // bootloader, and this one is still the device waited for after the touch.
    let options = SambaOptions {
        bootloader_devices: vec![DeviceFilter {
            vendor: 0x2341,
            product: None,
            serial_number: None,
        }],
        baud_rate: None,
    };
    let connection = connect_text(&transcript("connect-nano33ble"), None, options);
    assert_eq!(connection.result.unwrap().usb, identity(0x2341, 0x005a));
    connection.provider.finish().unwrap();
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
            erases_to_end: true,
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
            waited_for: Some("2341:005a".into()),
            reset: Some("touch-1200"),
        }
    );
    assert_eq!(error.class(), ErrorClass::Device);
    assert!(error.to_string().contains("double-tap"), "{error}");
    assert!(
        error.to_string().contains("after the 1200-baud touch"),
        "{error}"
    );
    provider.finish().unwrap();
}

#[test]
fn no_bootloader_device_after_custom_steps() {
    // The message names the reset that ran, not the touch.
    let reset = ResetSequence::Custom(vec![nanoflash::manifest::ResetStep {
        dtr: false,
        rts: false,
        wait: Duration::ZERO,
    }]);
    let text = transcript("connect-no-bootloader-device").replace("! baud 1200\n", "");
    let connection = connect_text(&text, Some(reset), nano33ble());
    let error = connection.result.unwrap_err();
    assert_eq!(
        error,
        Error::NoBootloader {
            waited_for: Some("2341:005a".into()),
            reset: Some("custom"),
        }
    );
    let message = error.to_string();
    assert!(!message.contains("1200"), "{message}");
    assert!(
        message.contains("after the reset sequence \"custom\" of the board"),
        "{message}"
    );
    connection.provider.finish().unwrap();
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
        Error::NoBootloader {
            waited_for: None,
            reset: None
        }
    );
    provider.finish().unwrap();
}

#[test]
fn a_late_binary_mode_reply_is_no_version() {
    // The `\n\r` of N# arrives after its 100 ms and is read as the answer to V#: an empty
    // line, so the next attempt drains the version line and asks again.
    let text = transcript("connect-nano33ble-bootloader").replace(
        "> \"N#\"\n< \"\\n\\r\"\n> \"V#\"\n",
        "> \"N#\"\n> \"V#\"\n< \"\\n\\r\"\n< \"Arduino Bootloader (SAM-BA extended) 2.0 [Arduino:IKXYZ]\\n\\r\"\n> \"N#\"\n< \"\\n\\r\"\n> \"V#\"\n",
    );
    assert_eq!(text.matches("> \"V#\"").count(), 2, "{text}");
    let connection = connect_text(&text, None, nano33ble());
    assert_eq!(connection.result.unwrap().chip, "nrf52840");
    connection.provider.finish().unwrap();
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
        Error::NoBootloader {
            waited_for: None,
            reset: None
        }
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
        Error::NoBootloader {
            waited_for: None,
            reset: None
        }
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

/// The progress events and messages of a session.
#[derive(Default)]
struct Recorder {
    events: Vec<ProgressEvent>,
    messages: Vec<String>,
}

impl nanoflash::backend::Progress for Recorder {
    fn report(&mut self, event: ProgressEvent) {
        self.events.push(event);
    }

    fn message(&mut self, text: &str) {
        self.messages.push(text.to_string());
    }
}

fn event(stage: Stage, offset: u64, done: u64, total: u64) -> ProgressEvent {
    ProgressEvent {
        stage,
        offset,
        done,
        total,
    }
}

fn application() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/application.bin"))
        .unwrap()
}

/// `connect-<board>` followed by `text`: the Nano 33 BLE in its bootloader, or the MKR WiFi
/// 1010.
fn after_connect(board: &str, text: &str) -> String {
    let name = match board {
        "nano33ble" => "connect-nano33ble-bootloader",
        _ => "connect-mkrwifi1010",
    };
    let mut full = transcript(name);
    full.push_str(text);
    full
}

/// Connects over a replay of `text`, then runs `body`, which gets the backend and a progress
/// recorder; the replay ends when `body` returns.
fn run<T>(
    text: &str,
    body: impl FnOnce(&mut SambaBackend<'_, ReplayProvider>, &mut Recorder) -> T,
) -> (T, ReplayProvider, Recorder) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let mut recorder = Recorder::default();
    let result = {
        let mut backend =
            SambaBackend::new(port, &mut provider, None, SambaOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        body(&mut backend, &mut recorder)
    };
    (result, provider, recorder)
}

/// The lines of one buffered write: `S`, the bytes as `bytes`, the selection of the buffer and
/// the copy of `length` bytes to `address`.
fn copy_lines(buffer: u32, address: u32, length: usize, bytes: &str) -> String {
    format!(
        "> \"S{buffer:08X},{length:08X}#\" {bytes}\n! sleep 10\n> \"Y{buffer:08X},0#\"\n< \"Y\\n\\r\"\n> \"Y{address:08X},{length:08X}#\"\n< \"Y\\n\\r\"\n"
    )
}

fn hexadecimal(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The data the host wrote after each `S` command of a recorded session.
fn payloads(transcript: &Transcript) -> Vec<Vec<u8>> {
    let writes: Vec<Vec<u8>> = transcript
        .events
        .iter()
        .filter_map(|entry| match &entry.event {
            Event::Host(tokens) => Some(
                tokens
                    .iter()
                    .map(|token| match token {
                        HostToken::Byte(byte) => *byte,
                        HostToken::Any | HostToken::Count(_) => {
                            panic!("a recording has no wildcard")
                        }
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect();
    writes
        .windows(2)
        .filter(|pair| pair[0].starts_with(b"S"))
        .map(|pair| pair[1].clone())
        .collect()
}

#[test]
fn writes_and_verifies_a_nano_33_ble() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0x1_0000,
        data: &image,
    };
    let text = after_connect("nano33ble", &transcript("write-nano33ble"));
    let (result, provider, recorder) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))?;
        block_on(backend.reset_to_application())
    });
    result.unwrap();
    provider.finish().unwrap();
    // `X` erases nothing on the nRF52840: `Y` erases the pages it programs, inside `Writing`.
    assert_eq!(
        recorder.events,
        [
            event(Stage::Writing, 0x1_0000, 0x1000, 0x2000),
            event(Stage::Writing, 0x1_0000, 0x2000, 0x2000),
            event(Stage::Verifying, 0x1_0000, 0x1000, 0x2000),
            event(Stage::Verifying, 0x1_0000, 0x2000, 0x2000),
        ]
    );
    assert!(recorder.messages.is_empty(), "{:?}", recorder.messages);
}

#[test]
fn sends_the_image_through_the_buffer() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0x1_0000,
        data: &image,
    };
    let text = after_connect("nano33ble", &transcript("write-nano33ble"));
    let (port, provider) = replay(Transcript::parse(&text).unwrap());
    let (port, mut provider) = record(port, provider);
    {
        let mut backend =
            SambaBackend::new(port, &mut provider, None, SambaOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        block_on(backend.write(&[segment], &mut ())).unwrap();
    }
    assert_eq!(
        payloads(&provider.transcript()),
        [image[..0x1000].to_vec(), image[0x1000..].to_vec()]
    );
}

#[test]
fn writes_a_samd21_and_says_x_erases_to_the_end() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0x2000,
        data: &image,
    };
    let text = after_connect("mkrwifi1010", &transcript("write-mkrwifi1010"));
    let (result, provider, recorder) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))?;
        block_on(backend.reset_to_application())
    });
    result.unwrap();
    provider.finish().unwrap();
    assert_eq!(
        recorder.events[..3],
        [
            event(Stage::Erasing, 0x2000, 0, 0x3_E000),
            event(Stage::Erasing, 0x2000, 0x3_E000, 0x3_E000),
            event(Stage::Writing, 0x2000, 0x1000, 0x2000),
        ]
    );
    assert_eq!(recorder.messages.len(), 1, "{:?}", recorder.messages);
    let message = &recorder.messages[0];
    assert!(
        message.contains("0x2000") && message.contains("0x40000"),
        "{message}"
    );
}

#[test]
fn pads_the_last_chunk_and_checks_the_image_bytes_only() {
    let image: Vec<u8> = (0..5000_u32).map(|index| (index * 7 % 251) as u8).collect();
    let tail = &image[0x1000..];
    assert_eq!(tail.len(), 904);
    let checksums = |first: u32, second: u32| {
        format!(
            "> \"Z{first:08X},00001000#\"\n< \"Z0000{:04X}#\\n\\r\"\n> \"Z{second:08X},00000388#\"\n< \"Z0000{:04X}#\\n\\r\"\n",
            crc16(&image[..0x1000]),
            crc16(tail)
        )
    };
    // The nRF52840 pads the last chunk to a whole 4 KiB page.
    let mut text = String::from("> \"X00000000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x34, 0, 0x1000, "..4096"));
    let padding = hexadecimal(&[0xFF; 3192]);
    text.push_str(&copy_lines(
        0x34,
        0x1000,
        0x1000,
        &format!("{} {padding}", hexadecimal(tail)),
    ));
    text.push_str(&checksums(0, 0x1000));
    let segment = Segment {
        label: "application",
        offset: 0x1_0000,
        data: &image,
    };
    let (result, provider, _) = run(&after_connect("nano33ble", &text), |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))
    });
    result.unwrap();
    provider.finish().unwrap();
    // The SAMD21 pads it to 64 bytes: 960.
    let mut text = String::from("> \"X00002000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x2000_4034, 0x2000, 0x1000, "..4096"));
    let padding = hexadecimal(&[0xFF; 56]);
    text.push_str(&copy_lines(
        0x2000_4034,
        0x3000,
        0x3C0,
        &format!("{} {padding}", hexadecimal(tail)),
    ));
    text.push_str(&checksums(0x2000, 0x3000));
    let segment = Segment {
        label: "application",
        offset: 0x2000,
        data: &image,
    };
    let (result, provider, _) = run(&after_connect("mkrwifi1010", &text), |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))
    });
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn x_is_sent_once_per_session() {
    let data = [0xAB_u8; 0x1000];
    let at = |offset| Segment {
        label: "data",
        offset,
        data: &data,
    };
    // In offset order, in two calls: one X.
    let mut text = String::from("> \"X00000000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x34, 0, 0x1000, "..4096"));
    text.push_str(&copy_lines(0x34, 0x1_0000, 0x1000, "..4096"));
    let (result, provider, _) = run(&after_connect("nano33ble", &text), |backend, progress| {
        block_on(backend.write(&[at(0x1_0000)], progress))?;
        block_on(backend.write(&[at(0x2_0000)], progress))
    });
    result.unwrap();
    provider.finish().unwrap();
    // A segment below the first one erased: a second X (which erases nothing on the nRF52840).
    let mut text = String::from("> \"X00010000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x34, 0x1_0000, 0x1000, "..4096"));
    text.push_str("> \"X00000000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x34, 0, 0x1000, "..4096"));
    let (result, provider, _) = run(&after_connect("nano33ble", &text), |backend, progress| {
        block_on(backend.write(&[at(0x2_0000)], progress))?;
        block_on(backend.write(&[at(0x1_0000)], progress))
    });
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn a_samd21_write_below_written_images_is_refused() {
    // X would erase from 0x2000 to the end of flash, the image at 0x3000 with it.
    let data = [0xAB_u8; 0x1000];
    let at = |offset| Segment {
        label: if offset == 0x2000 { "low" } else { "high" },
        offset,
        data: &data,
    };
    let mut text = String::from("> \"X00003000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x2000_4034, 0x3000, 0x1000, "..4096"));
    let (result, provider, _) = run(&after_connect("mkrwifi1010", &text), |backend, progress| {
        block_on(backend.write(&[at(0x3000)], progress))?;
        block_on(backend.write(&[at(0x2000)], progress))
    });
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::ImageBelowWritten {
            chip: "samd21g18a".into(),
            label: "low".into(),
            offset: 0x2000,
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    provider.finish().unwrap();
    // In one call, the order is checked before the first X.
    let (result, provider, _) = run(&after_connect("mkrwifi1010", ""), |backend, progress| {
        block_on(backend.write(&[at(0x3000), at(0x2000)], progress))
    });
    assert!(
        matches!(result, Err(Error::ImageBelowWritten { offset: 0x2000, .. })),
        "{result:?}"
    );
    provider.finish().unwrap();
}

#[test]
fn crc_mismatch_is_a_verification_failure() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0x1_0000,
        data: &image,
    };
    let text = after_connect("nano33ble", &transcript("crc-mismatch"));
    let (result, provider, _) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress)).unwrap();
        let result = block_on(backend.verify(&[segment], progress));
        block_on(backend.reset_to_application()).unwrap();
        result
    });
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::VerificationFailed {
            label: "application".into(),
            offset: 0x1_1000,
            size: 0x1000,
            expected: "d5b0".into(),
            actual: "1234".into(),
            checksum: "CRC-16",
        }
    );
    assert_eq!(error.class(), ErrorClass::Verification);
    provider.finish().unwrap();
}

#[test]
fn malformed_replies_name_the_command() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0x1_0000,
        data: &image,
    };
    let text = after_connect("nano33ble", &transcript("copy-refused"));
    let (result, provider, _) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress))
    });
    assert_eq!(
        result.unwrap_err(),
        Error::InvalidResponse {
            command: "Y",
            message: "expected Y\\n\\r, got y".into()
        }
    );
    provider.finish().unwrap();
    // A checksum of two digits.
    let text = transcript("write-nano33ble");
    let text = &text[..text.find("< \"Z0000D5B0#").unwrap()];
    let text = after_connect("nano33ble", &format!("{text}< \"Z12#\\n\\r\"\n"));
    let (result, provider, _) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress))?;
        block_on(backend.verify(&[segment], progress))
    });
    match result.unwrap_err() {
        Error::InvalidResponse { command, message } => {
            assert_eq!(command, "Z");
            assert!(message.contains("Z12#"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    provider.finish().unwrap();
    // A selection of the buffer never answered.
    let text = after_connect(
        "nano33ble",
        "> \"X00000000#\"\n< \"X\\n\\r\"\n> \"S00000034,00001000#\" ..4096\n! sleep 10\n> \"Y00000034,0#\"\n",
    );
    let (result, provider, _) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress))
    });
    assert_eq!(result.unwrap_err(), Error::NoResponse { command: "Y" });
    provider.finish().unwrap();
    // A reply longer than 64 bytes.
    let text = after_connect(
        "nano33ble",
        &format!("> \"X00000000#\"\n< \"X{}\\n\\r\"\n", "x".repeat(70)),
    );
    let (result, _, _) = run(&text, |backend, progress| {
        block_on(backend.write(&[segment], progress))
    });
    assert!(
        matches!(result, Err(Error::InvalidResponse { command: "X", .. })),
        "{result:?}"
    );
}

#[test]
fn erasing_an_nrf52840_region_writes_erased_pages() {
    let mut text = copy_lines(0x34, 0xE_0000, 0x1000, "..4096");
    text.push_str(&copy_lines(0x34, 0xE_1000, 0x1000, "..4096"));
    let text = after_connect("nano33ble", &text);
    let (port, provider) = replay(Transcript::parse(&text).unwrap());
    let (port, mut provider) = record(port, provider);
    let mut recorder = Recorder::default();
    {
        let mut backend =
            SambaBackend::new(port, &mut provider, None, SambaOptions::default()).unwrap();
        block_on(Backend::connect(&mut backend)).unwrap();
        let region = Region {
            offset: 0xF_0000,
            size: 0x2000,
        };
        block_on(backend.erase(region, &mut recorder)).unwrap();
    }
    provider.inner().finish().unwrap();
    assert_eq!(
        payloads(&provider.transcript()),
        [vec![0xFF; 0x1000], vec![0xFF; 0x1000]]
    );
    // The orchestration reports the start and the end of the region: only the pages between.
    assert_eq!(
        recorder.events,
        [event(Stage::Erasing, 0xF_0000, 0x1000, 0x2000)]
    );
    assert!(recorder.messages.is_empty(), "{:?}", recorder.messages);
}

#[test]
fn erasing_a_samd21_region_is_one_x() {
    let data = [0xAB_u8; 0x1000];
    let segment = Segment {
        label: "data",
        offset: 0x9000,
        data: &data,
    };
    let mut text = String::from("> \"X00008000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&copy_lines(0x2000_4034, 0x9000, 0x1000, "..4096"));
    let (result, provider, recorder) =
        run(&after_connect("mkrwifi1010", &text), |backend, progress| {
            let region = Region {
                offset: 0x8000,
                size: 0x3_8000,
            };
            block_on(backend.erase(region, progress))?;
            block_on(backend.write(&[segment], progress))
        });
    result.unwrap();
    provider.finish().unwrap();
    assert_eq!(recorder.messages.len(), 1, "{:?}", recorder.messages);
    assert!(
        recorder.messages[0].contains("0x8000"),
        "{:?}",
        recorder.messages
    );
    // The orchestration reports the erase itself.
    assert!(
        recorder
            .events
            .iter()
            .all(|event| event.stage == Stage::Writing),
        "{:?}",
        recorder.events
    );
}

#[test]
fn a_samd21_region_must_reach_the_end_of_flash() {
    // X erases to the end of flash: 0x9000..0x40000 would go too.
    let (result, provider, _) = run(&after_connect("mkrwifi1010", ""), |backend, progress| {
        let region = Region {
            offset: 0x8000,
            size: 0x1000,
        };
        block_on(backend.erase(region, progress))
    });
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::EraseShortOfFlashEnd {
            chip: "samd21g18a".into(),
            entry: "region at 0x8000".into(),
            offset: 0x8000,
            end: 0x9000,
            flash_size: 0x4_0000,
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    provider.finish().unwrap();
}

#[test]
fn resets_a_samd21_with_aircr() {
    let text = after_connect("mkrwifi1010", &transcript("reset-samd"));
    let (result, provider, _) = run(&text, |backend, _| block_on(backend.reset_to_application()));
    result.unwrap();
    provider.finish().unwrap();
    // The board may leave before the write completes.
    let text = after_connect("nano33ble", "! disconnect\n");
    let (result, provider, _) = run(&text, |backend, _| block_on(backend.reset_to_application()));
    result.unwrap();
    provider.finish().unwrap();
}

#[test]
fn below_the_application_is_refused_before_sending() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0,
        data: &image,
    };
    let (result, provider, _) = run(&after_connect("nano33ble", ""), |backend, progress| {
        let write = block_on(backend.write(&[segment], progress));
        let verify = block_on(backend.verify(&[segment], progress));
        let region = Region {
            offset: 0x8000,
            size: 0x1000,
        };
        let erase = block_on(backend.erase(region, progress));
        (write, verify, erase)
    });
    let below = |what: &str, offset| Error::BelowWritableFlash {
        what: what.into(),
        offset,
        writable_from: 0x1_0000,
    };
    assert_eq!(result.0, Err(below("image \"application\"", 0)));
    assert_eq!(result.1, Err(below("image \"application\"", 0)));
    assert_eq!(result.2, Err(below("erase region at 0x8000", 0x8000)));
    provider.finish().unwrap();
}

#[test]
fn past_the_flash_is_refused_before_sending() {
    let image = application();
    let segment = Segment {
        label: "application",
        offset: 0xF_F000,
        data: &image,
    };
    let (result, provider, _) = run(&after_connect("nano33ble", ""), |backend, progress| {
        block_on(backend.write(&[segment], progress))
    });
    assert_eq!(
        result.unwrap_err(),
        Error::OutsideDetectedFlash {
            what: "image \"application\"".into(),
            end: 0x10_1000,
            flash_size: 0x10_0000,
        }
    );
    provider.finish().unwrap();
}

#[test]
fn nothing_is_done_before_connecting() {
    let (port, mut provider) = replay(Transcript::parse("").unwrap());
    let mut backend =
        SambaBackend::new(port, &mut provider, None, SambaOptions::default()).unwrap();
    let segment = Segment {
        label: "application",
        offset: 0x1_0000,
        data: &[0; 4],
    };
    let region = Region {
        offset: 0x1_0000,
        size: 0x1000,
    };
    let none = Err(Error::NotConnected {
        protocol: Protocol::Samba,
    });
    assert_eq!(block_on(backend.write(&[segment], &mut ())), none);
    assert_eq!(block_on(backend.verify(&[segment], &mut ())), none);
    assert_eq!(block_on(backend.erase(region, &mut ())), none);
    assert_eq!(block_on(backend.reset_to_application()), none);
}

fn fixture(relative: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The text of `tests/boards/<name>.toml`.
fn board_text(name: &str) -> String {
    String::from_utf8(fixture(&format!("boards/{name}.toml"))).unwrap()
}

fn board(name: &str) -> Manifest {
    Manifest::from_toml(&board_text(name)).unwrap()
}

/// The layouts and images of the fixture boards, under the paths their manifests use.
fn fixture_source() -> MemorySource {
    let mut files = BTreeMap::new();
    for path in [
        "../layouts/nrf52840-arduino.toml",
        "../images/application.bin",
    ] {
        files.insert(path.to_string(), fixture(&path[3..]));
    }
    MemorySource { files }
}

/// Flashes `manifest` over a replay of `text`.
fn run_flash(
    manifest: &Manifest,
    text: &str,
    source: &mut MemorySource,
    options: &FlashOptions,
    progress: &mut Recorder,
) -> (Result<FlashReport, Error>, ReplayProvider) {
    let (port, mut provider) = replay(Transcript::parse(text).unwrap());
    let hooks = Hooks {
        source,
        verifier: &mut (),
        progress,
    };
    let result = block_on(flash(manifest, port, &mut provider, hooks, options));
    (result, provider)
}

fn images(report: &FlashReport) -> Vec<(&str, u64, u64)> {
    report
        .images
        .iter()
        .map(|image| (image.label.as_str(), image.offset, image.size))
        .collect()
}

fn stages(recorder: &Recorder) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();
    for event in &recorder.events {
        if stages.last() != Some(&event.stage) {
            stages.push(event.stage);
        }
    }
    stages
}

#[test]
fn flashes_the_nano_33_ble_fixture() {
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &board("nano33ble"),
        &transcript("flash-nano33ble"),
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut recorder,
    );
    let report = result.unwrap();
    assert_eq!(report.board, "arduino-nano-33-ble");
    assert_eq!(report.chip.chip, "nrf52840");
    assert_eq!(report.chip.writable_from, Some(0x1_0000));
    assert!(!report.chip.erases_to_end);
    assert_eq!(images(&report), [("application", 0x1_0000, 8192)]);
    assert!(report.verified);
    assert!(report.warnings.is_empty());
    provider.finish().unwrap();
    assert_eq!(
        stages(&recorder),
        [
            Stage::Connecting,
            Stage::Writing,
            Stage::Verifying,
            Stage::Resetting
        ]
    );
    assert!(recorder.messages.is_empty(), "{:?}", recorder.messages);
}

#[test]
fn flashes_an_mkr_wifi_1010() {
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &board("mkr-wifi-1010"),
        &transcript("flash-mkrwifi1010"),
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut recorder,
    );
    let report = result.unwrap();
    assert_eq!(report.board, "arduino-mkr-wifi-1010");
    assert_eq!(report.chip.chip, "samd21g18a");
    assert_eq!(report.chip.writable_from, Some(0x2000));
    assert!(report.chip.erases_to_end);
    assert_eq!(images(&report), [("image at 0x2000", 0x2000, 8192)]);
    assert!(report.verified);
    provider.finish().unwrap();
    // The X of the write erases to the end of flash, and says so as it happens and in the report.
    assert_eq!(recorder.messages.len(), 1, "{:?}", recorder.messages);
    assert_eq!(
        report.warnings,
        [Warning::ErasesToEnd {
            from: 0x2000,
            to: 0x4_0000,
            partitions: Vec::new(),
        }]
    );
}

#[test]
fn the_erase_to_the_end_of_flash_names_the_partitions_it_takes() {
    let manifest = Manifest::from_toml(&board_text("mkr-wifi-1010").replace(
        "reset = \"touch-1200\"\n",
        "reset = \"touch-1200\"\nlayout = \"../layouts/samd21.toml\"\n",
    ))
    .unwrap();
    let mut source = fixture_source();
    source.files.insert(
        "../layouts/samd21.toml".into(),
        b"[flash]\nsize = \"256 KiB\"\n\n\
          [[partition]]\nname = \"bootloader\"\nrole = \"bootloader\"\noffset = 0x0\nsize = 0x2000\n\n\
          [[partition]]\nname = \"application\"\nrole = \"firmware\"\nsize = \"remaining\"\n\n\
          [[partition]]\nname = \"storage\"\nrole = \"storage\"\noffset = 0x3C000\nsize = \"16 KiB\"\n"
            .to_vec(),
    );
    let (result, provider) = run_flash(
        &manifest,
        &transcript("flash-mkrwifi1010"),
        &mut source,
        &FlashOptions::default(),
        &mut Recorder::default(),
    );
    let report = result.unwrap();
    provider.finish().unwrap();
    assert_eq!(
        report.warnings,
        [Warning::ErasesToEnd {
            from: 0x2000,
            to: 0x4_0000,
            partitions: vec!["storage".into()],
        }]
    );
    assert!(
        report.warnings[0]
            .to_string()
            .ends_with("including the partitions \"storage\""),
        "{}",
        report.warnings[0]
    );
}

#[test]
fn crc_mismatch_resets_and_fails() {
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &board("nano33ble"),
        &transcript("flash-nano33ble-crc-mismatch"),
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut recorder,
    );
    let error = result.unwrap_err();
    assert!(
        matches!(
            error,
            Error::VerificationFailed {
                offset: 0x1_1000,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.class(), ErrorClass::Verification);
    // K# was sent.
    provider.finish().unwrap();
    assert_eq!(stages(&recorder).last(), Some(&Stage::Resetting));
}

#[test]
fn images_below_the_application_are_refused_first() {
    let mut text = board_text("nano33ble");
    text.push_str("\n[[board.image]]\npartition = \"bootloader\"\npath = \"bootloader.bin\"\n");
    let manifest = Manifest::from_toml(&text).unwrap();
    let mut source = fixture_source();
    source
        .files
        .insert("bootloader.bin".to_string(), vec![0xA5; 16]);
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &manifest,
        &transcript("flash-below-application"),
        &mut source,
        &FlashOptions::default(),
        &mut recorder,
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        Error::BelowWritableFlash {
            what: "image \"bootloader\"".into(),
            offset: 0,
            writable_from: 0x1_0000,
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    // No X: only K# after connecting.
    provider.finish().unwrap();
    assert_eq!(stages(&recorder), [Stage::Connecting, Stage::Resetting]);
}

/// The lines of `Z` over `data` at `address`, one 4096-byte chunk at a time.
fn checksum_lines(address: u32, data: &[u8]) -> String {
    let mut text = String::new();
    for (index, chunk) in data.chunks(0x1000).enumerate() {
        let chunk_address = address + u32::try_from(index * 0x1000).unwrap();
        writeln!(
            text,
            "> \"Z{chunk_address:08X},{:08X}#\"\n< \"Z{:08X}#\\n\\r\"",
            chunk.len(),
            crc16(chunk)
        )
        .unwrap();
    }
    text
}

/// The lines of the buffered writes of `data` at `address` on a SAMD21.
fn samd21_copies(address: u32, data: &[u8]) -> String {
    let mut text = String::new();
    for (index, chunk) in data.chunks(0x1000).enumerate() {
        let chunk_address = address + u32::try_from(index * 0x1000).unwrap();
        text.push_str(&copy_lines(
            0x2000_4034,
            chunk_address,
            chunk.len().next_multiple_of(0x40),
            &format!("..{}", chunk.len().next_multiple_of(0x40)),
        ));
    }
    text
}

const SAMD21_RESET: &str = "> \"WE000ED0C,05FA0004#\"\n";

#[test]
fn erase_all_starts_at_the_application() {
    let image = application();
    let mut text = transcript("connect-mkrwifi1010");
    // Erase all: one X from the application start; the write needs no second X.
    text.push_str("> \"X00002000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&samd21_copies(0x2000, &image));
    text.push_str(&checksum_lines(0x2000, &image));
    text.push_str(SAMD21_RESET);
    let options = FlashOptions {
        erase_all: true,
        ..FlashOptions::default()
    };
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &board("mkr-wifi-1010"),
        &text,
        &mut fixture_source(),
        &options,
        &mut recorder,
    );
    result.unwrap();
    provider.finish().unwrap();
    assert_eq!(
        recorder.events[1..3],
        [
            event(Stage::Erasing, 0x2000, 0, 0x3_E000),
            event(Stage::Erasing, 0x2000, 0x3_E000, 0x3_E000),
        ]
    );
    assert_eq!(recorder.messages.len(), 1, "{:?}", recorder.messages);
}

/// The MKR WiFi 1010 with `image.bin` at 0x2000 instead of the fixture image.
fn mkr_wifi_1010_with(image: &[u8]) -> (Manifest, MemorySource) {
    let text = board_text("mkr-wifi-1010").replace("../images/application.bin", "image.bin");
    let mut source = MemorySource::default();
    source.files.insert("image.bin".to_string(), image.to_vec());
    (Manifest::from_toml(&text).unwrap(), source)
}

#[test]
fn image_ending_at_the_end_of_flash() {
    // 0x2000 + 0x3E000 = 0x40000, the end of the SAMD21G18A's flash.
    let image: Vec<u8> = (0..0x3_E000_u32)
        .map(|index| (index / 0x1000) as u8)
        .collect();
    let (manifest, mut source) = mkr_wifi_1010_with(&image);
    let mut text = transcript("connect-mkrwifi1010");
    text.push_str("> \"X00002000#\"\n< \"X\\n\\r\"\n");
    text.push_str(&samd21_copies(0x2000, &image));
    text.push_str(&checksum_lines(0x2000, &image));
    text.push_str(SAMD21_RESET);
    let (result, provider) = run_flash(
        &manifest,
        &text,
        &mut source,
        &FlashOptions::default(),
        &mut Recorder::default(),
    );
    assert_eq!(
        images(&result.unwrap()),
        [("image at 0x2000", 0x2000, 0x3_E000)]
    );
    provider.finish().unwrap();

    // One byte more: refused once the chip is known, before any X.
    let mut longer = image;
    longer.push(0);
    let (manifest, mut source) = mkr_wifi_1010_with(&longer);
    let mut text = transcript("connect-mkrwifi1010");
    text.push_str(SAMD21_RESET);
    let (result, provider) = run_flash(
        &manifest,
        &text,
        &mut source,
        &FlashOptions::default(),
        &mut Recorder::default(),
    );
    assert_eq!(
        result.unwrap_err(),
        Error::OutsideDetectedFlash {
            what: "image \"image at 0x2000\"".into(),
            end: 0x4_0001,
            flash_size: 0x4_0000,
        }
    );
    provider.finish().unwrap();
}

#[test]
fn detects_a_nano_33_ble_without_a_manifest() {
    // The protocol comes from the vendor 0x2341, the bootloader's identity from the built-in
    // rule (0x805a to 0x005a).
    let (port, mut provider) = replay(Transcript::parse(&transcript("detect-nano33ble")).unwrap());
    let chip = block_on(detect(port, &mut provider, None, &mut ())).unwrap();
    assert_eq!(chip.protocol, Protocol::Samba);
    assert_eq!(chip.chip, "nrf52840");
    assert_eq!(chip.usb, identity(0x2341, 0x005a));
    provider.finish().unwrap();
}

/// Flashes the MKR WiFi 1010 with `erase` appended to its board, over `connect-mkrwifi1010`
/// and the reset only.
fn flash_mkr_wifi_1010_erasing(erase: &str, erase_all: bool) -> (Error, Recorder) {
    let mut text = board_text("mkr-wifi-1010");
    text.push_str(erase);
    let manifest = Manifest::from_toml(&text).unwrap();
    let mut transcript_text = transcript("connect-mkrwifi1010");
    transcript_text.push_str(SAMD21_RESET);
    let options = FlashOptions {
        erase_all,
        ..FlashOptions::default()
    };
    let mut recorder = Recorder::default();
    let (result, provider) = run_flash(
        &manifest,
        &transcript_text,
        &mut fixture_source(),
        &options,
        &mut recorder,
    );
    // No X was sent.
    provider.finish().unwrap();
    (result.unwrap_err(), recorder)
}

#[test]
fn erase_all_and_a_short_erase_entry_are_refused_before_erasing() {
    let (error, recorder) =
        flash_mkr_wifi_1010_erasing("\n[[board.erase]]\noffset = 0x8000\nsize = 0x1000\n", true);
    assert_eq!(
        error,
        Error::EraseShortOfFlashEnd {
            chip: "samd21g18a".into(),
            entry: "region at 0x8000".into(),
            offset: 0x8000,
            end: 0x9000,
            flash_size: 0x4_0000,
        }
    );
    assert_eq!(error.class(), ErrorClass::Input);
    assert!(
        error
            .to_string()
            .contains("the erase entry \"region at 0x8000\""),
        "{error}"
    );
    assert_eq!(stages(&recorder), [Stage::Connecting, Stage::Resetting]);
}

#[test]
fn a_short_erase_entry_after_one_reaching_the_end_is_refused_before_erasing() {
    let (error, recorder) = flash_mkr_wifi_1010_erasing(
        "\n[[board.erase]]\noffset = 0x10000\nsize = 0x30000\n\n\
         [[board.erase]]\noffset = 0x8000\nsize = 0x1000\n",
        false,
    );
    assert_eq!(
        error,
        Error::EraseShortOfFlashEnd {
            chip: "samd21g18a".into(),
            entry: "region at 0x8000".into(),
            offset: 0x8000,
            end: 0x9000,
            flash_size: 0x4_0000,
        }
    );
    assert_eq!(stages(&recorder), [Stage::Connecting, Stage::Resetting]);
}

#[test]
fn flash_fills_the_bootloader_devices_from_the_manifest() {
    // Not an Arduino vendor: without the manifest's bootloader-devices the backend could not tell
    // which device to wait for after the touch (`UnknownBootloaderDevice`).
    let manifest = Manifest::from_toml(
        "[[board]]\nidentifier = \"other-samd21\"\nprotocol = \"samba\"\nchip = \"samd21g18a\"\n\
         application-devices = [{ vendor = 0x1209, product = 0x8001 }]\n\
         bootloader-devices = [{ vendor = 0x1209, product = 0x0001 }]\n\
         [[board.image]]\noffset = 0x2000\npath = \"../images/application.bin\"\n",
    )
    .unwrap();
    let text = transcript("flash-mkrwifi1010")
        .replace("! identity 2341:8054", "! identity 1209:8001")
        .replace("! reopen 2341:0054", "! reopen 1209:0001");
    let (result, provider) = run_flash(
        &manifest,
        &text,
        &mut fixture_source(),
        &FlashOptions::default(),
        &mut Recorder::default(),
    );
    let report = result.unwrap();
    assert_eq!(report.board, "other-samd21");
    assert_eq!(report.chip.usb, identity(0x1209, 0x0001));
    provider.finish().unwrap();
}
