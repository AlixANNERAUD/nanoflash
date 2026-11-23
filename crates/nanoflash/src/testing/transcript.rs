//! The text format of a serial session.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use crate::transport::{Duration, Signals, UsbIdentity};

/// A byte the host writes, or a wildcard for some bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostToken {
    /// This exact byte.
    Byte(u8),
    /// One or more bytes, up to the next literal byte of the line.
    Any,
    /// Exactly this many bytes, of any value.
    Count(usize),
}

/// Why a `PortProvider::open` fails in a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFailure {
    /// No port matches the filter.
    NotFound,
    /// The user did not grant access to a port.
    NotGranted,
}

/// One thing that happened on the port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Bytes the host writes.
    Host(Vec<HostToken>),
    /// Bytes the device sends.
    Device(Vec<u8>),
    /// `set_signals`.
    Signals(Signals),
    /// `set_baud_rate`.
    BaudRate(u32),
    /// `sleep`.
    Sleep(Duration),
    /// A read that returned 0.
    Timeout,
    /// The next read or write fails with `Disconnected`.
    Disconnect,
    /// `PortProvider::open` succeeded with a port of this identity (`None` when it has none).
    Reopen(Option<UsbIdentity>),
    /// The next `PortProvider::open` fails like this.
    OpenFails(OpenFailure),
}

/// An event and the line it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The 1-based line of the transcript.
    pub line: usize,
    /// What happened.
    pub event: Event,
}

/// A serial session: the identity of the initial port and the events in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Transcript {
    /// The identity of the initial port.
    pub identity: Option<UsbIdentity>,
    /// The events, in order.
    pub events: Vec<Entry>,
}

/// A line of a transcript that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptError {
    /// The 1-based line.
    pub line: usize,
    /// What is wrong with it.
    pub message: String,
}

impl fmt::Display for TranscriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "transcript line {}: {}", self.line, self.message)
    }
}

impl core::error::Error for TranscriptError {}

fn parse_byte(token: &str) -> Result<u8, String> {
    if !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("`{token}` is not a hexadecimal byte"));
    }
    if token.len() % 2 == 1 {
        return Err(format!("`{token}` has an odd number of hexadecimal digits"));
    }
    if token.len() != 2 {
        return Err(format!(
            "`{token}` is not one byte, write two digits per byte"
        ));
    }
    u8::from_str_radix(token, 16).map_err(|_| format!("`{token}` is not a hexadecimal byte"))
}

/// The largest number of bytes a `..N` wildcard may take.
const MAXIMUM_COUNT: usize = 16_777_216;

/// A piece of a `>` or `<` line.
enum Piece<'a> {
    /// A word between whitespace.
    Word(&'a str),
    /// The bytes of a quoted text.
    Text(Vec<u8>),
}

fn parse_quoted(characters: &mut core::str::Chars<'_>) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let character = characters
            .next()
            .ok_or_else(|| "a quoted text is not closed, add a closing `\"`".to_string())?;
        match character {
            '"' => return Ok(bytes),
            '\\' => match characters.next() {
                Some('n') => bytes.push(b'\n'),
                Some('r') => bytes.push(b'\r'),
                Some('\\') => bytes.push(b'\\'),
                Some('"') => bytes.push(b'"'),
                Some('x') => {
                    let digits: String = characters.by_ref().take(2).collect();
                    if digits.len() != 2 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(format!(
                            "`\\x{digits}` is not an escape, write `\\x` and two hexadecimal digits"
                        ));
                    }
                    bytes.push(
                        u8::from_str_radix(&digits, 16)
                            .map_err(|_| format!("`\\x{digits}` is not an escape"))?,
                    );
                }
                Some(other) => {
                    return Err(format!(
                        "`\\{other}` is not an escape, use `\\n`, `\\r`, `\\\\`, `\\\"` or `\\xNN`"
                    ));
                }
                None => return Err("a quoted text is not closed, add a closing `\"`".to_string()),
            },
            ' '..='~' => bytes.push(character as u8),
            other => {
                return Err(format!(
                    "`{}` is not printable ASCII in a quoted text, use `\\xNN`",
                    other.escape_default()
                ));
            }
        }
    }
}

/// Splits the text of a `>` or `<` line into words and quoted texts.
fn pieces(text: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut found = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        let Some(first) = rest.chars().next() else {
            return Ok(found);
        };
        if first == '"' {
            let mut characters = rest.get(1..).unwrap_or("").chars();
            found.push(Piece::Text(parse_quoted(&mut characters)?));
            rest = characters.as_str();
            if rest
                .chars()
                .next()
                .is_some_and(|next| !next.is_whitespace())
            {
                return Err(
                    "a quoted text must be followed by a space or the end of the line".to_string(),
                );
            }
        } else {
            let length = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let (word, tail) = rest.split_at(length);
            found.push(Piece::Word(word));
            rest = tail;
        }
    }
}

fn parse_count(digits: &str) -> Result<usize, String> {
    let invalid = || {
        format!("`..{digits}` is not a count, write `..` and a number from 1 to {MAXIMUM_COUNT}")
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    match digits.parse::<usize>() {
        Ok(count) if (1..=MAXIMUM_COUNT).contains(&count) => Ok(count),
        _ => Err(invalid()),
    }
}

fn parse_host(text: &str) -> Result<Vec<HostToken>, String> {
    let mut tokens = Vec::new();
    for piece in pieces(text)? {
        match piece {
            Piece::Text(bytes) => tokens.extend(bytes.into_iter().map(HostToken::Byte)),
            Piece::Word("..") => tokens.push(HostToken::Any),
            Piece::Word(word) => match word.strip_prefix("..") {
                Some(digits) => tokens.push(HostToken::Count(parse_count(digits)?)),
                None => tokens.push(HostToken::Byte(parse_byte(word)?)),
            },
        }
    }
    if tokens.is_empty() {
        return Err("a `>` line needs at least one byte".to_string());
    }
    for (index, token) in tokens.iter().enumerate() {
        if *token == HostToken::Any && !matches!(tokens.get(index + 1), Some(HostToken::Byte(_))) {
            return Err("`..` must be followed by a literal byte on its line".to_string());
        }
    }
    Ok(tokens)
}

fn parse_device(text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for piece in pieces(text)? {
        match piece {
            Piece::Text(text) => bytes.extend(text),
            Piece::Word(word) if word.starts_with("..") => {
                return Err("`..` is only allowed in a `>` line".to_string());
            }
            Piece::Word(word) => bytes.push(parse_byte(word)?),
        }
    }
    if bytes.is_empty() {
        return Err("a `<` line needs at least one byte".to_string());
    }
    Ok(bytes)
}

fn parse_hex_u16(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(text, 16).ok()
}

/// `303a:1001` followed by an optional serial number.
fn parse_identity(words: &[&str]) -> Result<UsbIdentity, String> {
    let identifier = words.first().copied().unwrap_or("");
    let invalid = || format!("`{identifier}` is not an identity like 303a:1001");
    let (vendor, product) = identifier.split_once(':').ok_or_else(invalid)?;
    let vendor = parse_hex_u16(vendor).ok_or_else(invalid)?;
    let product = parse_hex_u16(product).ok_or_else(invalid)?;
    let serial_number = match words.get(1..) {
        Some(rest) if !rest.is_empty() => Some(rest.join(" ")),
        _ => None,
    };
    Ok(UsbIdentity {
        vendor,
        product,
        serial_number,
    })
}

fn parse_flag(text: &str, name: &str) -> Result<bool, String> {
    match text
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('='))
    {
        Some("1") => Ok(true),
        Some("0") => Ok(false),
        _ => Err(format!("expected `{name}=0` or `{name}=1`, got `{text}`")),
    }
}

enum Directive {
    Identity(UsbIdentity),
    Event(Event),
}

fn parse_directive(text: &str, first: bool) -> Result<Directive, String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (name, arguments) = match words.split_first() {
        Some((name, arguments)) => (*name, arguments),
        None => return Err("a `!` line needs a directive".to_string()),
    };
    let no_arguments = |event: Event| {
        if arguments.is_empty() {
            Ok(Directive::Event(event))
        } else {
            Err(format!("`{name}` takes no argument"))
        }
    };
    match name {
        "identity" => {
            if !first {
                return Err("`identity` must be the first event".to_string());
            }
            parse_identity(arguments).map(Directive::Identity)
        }
        "signals" => match arguments {
            [first, second] => {
                let (dtr, rts) = match (first.starts_with("dtr"), second.starts_with("dtr")) {
                    (true, false) => (first, second),
                    (false, true) => (second, first),
                    _ => return Err("expected `dtr=` and `rts=`".to_string()),
                };
                Ok(Directive::Event(Event::Signals(Signals {
                    dtr: parse_flag(dtr, "dtr")?,
                    rts: parse_flag(rts, "rts")?,
                })))
            }
            _ => Err("expected `signals dtr=0 rts=1`".to_string()),
        },
        "baud" => match arguments {
            [value] => value
                .parse::<u32>()
                .map(|baud| Directive::Event(Event::BaudRate(baud)))
                .map_err(|_| format!("`{value}` is not a baud rate")),
            _ => Err("expected `baud <rate>`".to_string()),
        },
        "sleep" => match arguments {
            [value] => value
                .parse::<u64>()
                .map(|milliseconds| {
                    Directive::Event(Event::Sleep(Duration::from_millis(milliseconds)))
                })
                .map_err(|_| format!("`{value}` is not a number of milliseconds")),
            _ => Err("expected `sleep <milliseconds>`".to_string()),
        },
        "timeout" => no_arguments(Event::Timeout),
        "disconnect" => no_arguments(Event::Disconnect),
        "reopen" => {
            if arguments.is_empty() {
                Ok(Directive::Event(Event::Reopen(None)))
            } else {
                parse_identity(arguments)
                    .map(|identity| Directive::Event(Event::Reopen(Some(identity))))
            }
        }
        "open-fails" => match arguments {
            ["not-found"] => Ok(Directive::Event(Event::OpenFails(OpenFailure::NotFound))),
            ["not-granted"] => Ok(Directive::Event(Event::OpenFails(OpenFailure::NotGranted))),
            _ => Err("expected `open-fails not-found` or `open-fails not-granted`".to_string()),
        },
        other => Err(format!("unknown directive `{other}`")),
    }
}

impl Transcript {
    /// Reads a transcript from its text form.
    pub fn parse(text: &str) -> Result<Transcript, TranscriptError> {
        let mut identity = None;
        let mut events = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let error = |message: String| TranscriptError { line, message };
            let event = if let Some(rest) = trimmed.strip_prefix('>') {
                Event::Host(parse_host(rest).map_err(error)?)
            } else if let Some(rest) = trimmed.strip_prefix('<') {
                Event::Device(parse_device(rest).map_err(error)?)
            } else if let Some(rest) = trimmed.strip_prefix('!') {
                let first = events.is_empty() && identity.is_none();
                match parse_directive(rest, first).map_err(error)? {
                    Directive::Identity(found) => {
                        identity = Some(found);
                        continue;
                    }
                    Directive::Event(event) => event,
                }
            } else {
                return Err(error("a line starts with `>`, `<`, `!` or `#`".to_string()));
            };
            events.push(Entry { line, event });
        }
        Ok(Transcript { identity, events })
    }
}

fn write_identity(formatter: &mut fmt::Formatter<'_>, identity: &UsbIdentity) -> fmt::Result {
    write!(
        formatter,
        "{:04x}:{:04x}",
        identity.vendor, identity.product
    )?;
    if let Some(serial_number) = &identity.serial_number {
        write!(formatter, " {serial_number}")?;
    }
    Ok(())
}

impl fmt::Display for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Host(tokens) => {
                formatter.write_str(">")?;
                for token in tokens {
                    match token {
                        HostToken::Byte(byte) => write!(formatter, " {byte:02x}")?,
                        HostToken::Any => formatter.write_str(" ..")?,
                        HostToken::Count(count) => write!(formatter, " ..{count}")?,
                    }
                }
                Ok(())
            }
            Event::Device(bytes) => {
                formatter.write_str("<")?;
                for byte in bytes {
                    write!(formatter, " {byte:02x}")?;
                }
                Ok(())
            }
            Event::Signals(signals) => write!(
                formatter,
                "! signals dtr={} rts={}",
                u8::from(signals.dtr),
                u8::from(signals.rts)
            ),
            Event::BaudRate(baud_rate) => write!(formatter, "! baud {baud_rate}"),
            Event::Sleep(duration) => write!(formatter, "! sleep {}", duration.as_millis()),
            Event::Timeout => formatter.write_str("! timeout"),
            Event::Disconnect => formatter.write_str("! disconnect"),
            Event::OpenFails(OpenFailure::NotFound) => {
                formatter.write_str("! open-fails not-found")
            }
            Event::OpenFails(OpenFailure::NotGranted) => {
                formatter.write_str("! open-fails not-granted")
            }
            Event::Reopen(None) => formatter.write_str("! reopen"),
            Event::Reopen(Some(identity)) => {
                formatter.write_str("! reopen ")?;
                write_identity(formatter, identity)
            }
        }
    }
}

impl fmt::Display for Transcript {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(identity) = &self.identity {
            formatter.write_str("! identity ")?;
            write_identity(formatter, identity)?;
            formatter.write_str("\n")?;
        }
        for entry in &self.events {
            writeln!(formatter, "{}", entry.event)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_KIND: &str = "# a comment\n\
        ! identity 303a:1001\n\
        > c0 00 08 .. c0\n\
        < c0 01 08 04 00 c0\n\
        ! signals dtr=1 rts=0\n\
        ! baud 460800\n\
        ! sleep 100\n\
        ! timeout\n\
        ! disconnect\n\
        ! reopen 303a:1001\n";

    #[test]
    fn parses_every_event_kind() {
        let transcript = Transcript::parse(EVERY_KIND).unwrap();
        let identity = UsbIdentity {
            vendor: 0x303a,
            product: 0x1001,
            serial_number: None,
        };
        assert_eq!(transcript.identity, Some(identity.clone()));
        let events: Vec<Event> = transcript
            .events
            .iter()
            .map(|entry| entry.event.clone())
            .collect();
        assert_eq!(
            events,
            [
                Event::Host(alloc::vec![
                    HostToken::Byte(0xc0),
                    HostToken::Byte(0),
                    HostToken::Byte(8),
                    HostToken::Any,
                    HostToken::Byte(0xc0)
                ]),
                Event::Device(alloc::vec![0xc0, 1, 8, 4, 0, 0xc0]),
                Event::Signals(Signals {
                    dtr: true,
                    rts: false
                }),
                Event::BaudRate(460_800),
                Event::Sleep(Duration::from_millis(100)),
                Event::Timeout,
                Event::Disconnect,
                Event::Reopen(Some(identity)),
            ]
        );
        assert_eq!(transcript.events[0].line, 3);
    }

    #[test]
    fn rejects_bad_lines() {
        for (text, line) in [
            ("< c0 0", 1),
            ("# a\n> c0 ..", 2),
            ("> c0\n! identity 303a:1001", 2),
            ("! wait 3", 1),
            ("< c0 .. c0", 1),
            ("> c0 zz", 1),
            ("> c0 .. ..", 1),
            ("hello", 1),
        ] {
            assert_eq!(Transcript::parse(text).unwrap_err().line, line, "{text}");
        }
    }

    #[test]
    fn display_round_trips() {
        let canonical = "! identity 303a:1001 serial 7\n\
            > c0 00 .. c0\n\
            < c0 01 c0\n\
            ! signals dtr=1 rts=0\n\
            ! baud 460800\n\
            ! sleep 100\n\
            ! timeout\n\
            ! disconnect\n\
            ! reopen\n\
            ! reopen 303a:1001\n";
        let transcript = Transcript::parse(canonical).unwrap();
        assert_eq!(transcript.to_string(), canonical);
        assert_eq!(
            Transcript::parse(&transcript.to_string()).unwrap(),
            transcript
        );
    }

    #[test]
    fn quoted_text_is_ascii_bytes() {
        let host = Transcript::parse("> \"N#\"").unwrap();
        assert_eq!(
            host.events[0].event,
            Event::Host(alloc::vec![HostToken::Byte(0x4e), HostToken::Byte(0x23)])
        );
        let device = Transcript::parse("< \"Y\\n\\r\" 00\n< \"a \\\"b\\\" \\\\ \\x41\"").unwrap();
        assert_eq!(
            device.events[0].event,
            Event::Device(alloc::vec![0x59, 0x0a, 0x0d, 0x00])
        );
        assert_eq!(
            device.events[1].event,
            Event::Device(b"a \"b\" \\ A".to_vec())
        );
        let mixed = Transcript::parse("< \"Z0000D5B0#\" 0a 0d").unwrap();
        assert_eq!(
            mixed.events[0].event,
            Event::Device(b"Z0000D5B0#\n\r".to_vec())
        );
    }

    #[test]
    fn rejects_bad_text_and_counts() {
        for (text, line) in [
            ("> \"N#", 1),
            ("> \"\\q\"", 1),
            ("< \"\u{e9}\"", 1),
            ("< ..4", 1),
            ("> ..0", 1),
            ("> ..x", 1),
            ("> ..16777217", 1),
            ("> \"a\"b", 1),
            ("! open-fails maybe", 1),
            ("! open-fails", 1),
        ] {
            assert_eq!(Transcript::parse(text).unwrap_err().line, line, "{text}");
        }
    }

    #[test]
    fn display_keeps_counts_and_open_failures() {
        let canonical = "> 53 ..4096\n\
            > ..3 59\n\
            ! open-fails not-found\n\
            ! open-fails not-granted\n";
        let transcript = Transcript::parse(canonical).unwrap();
        assert_eq!(transcript.to_string(), canonical);
        let quoted = Transcript::parse("> \"N#\"\n< \"Y\" 0a").unwrap();
        assert_eq!(quoted.to_string(), "> 4e 23\n< 59 0a\n");
        assert_eq!(Transcript::parse(&quoted.to_string()).unwrap(), quoted);
    }
}
