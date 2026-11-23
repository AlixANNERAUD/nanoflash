// Intel HEX as Intel's "Hexadecimal Object File Format Specification" (1988) defines it; written
// for nanoflash, no code taken from elsewhere.
//! Reading Intel HEX text into the flat image it describes.
//!
//! Arduino, PlatformIO and `objcopy` write firmware as Intel HEX (`.hex`); the decoder turns it
//! into the bytes from its lowest address to its highest, `0xFF` in the gaps, so every protocol
//! writes it as it writes a binary image. It is `no_std`, and every allocation it makes is
//! fallible: the image's after its span is checked, the records' as they are read.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// The flat image an Intel HEX file describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntelHexImage {
    /// The lowest address of a data record.
    pub start: u64,
    /// The bytes from `start` up to the highest address of a data record (inclusive), `0xFF`
    /// where no record gives a byte.
    pub bytes: Vec<u8>,
}

/// Why Intel HEX text cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntelHexError {
    /// The text is not a valid image.
    Invalid {
        /// The 1-based line the problem is on; `None` when it is about the whole file.
        line: Option<usize>,
        /// What is wrong.
        message: String,
    },
    /// There is not enough memory for the image.
    OutOfMemory {
        /// The size of the image in bytes.
        size: u64,
    },
}

impl IntelHexError {
    fn invalid(line: Option<usize>, message: impl Into<String>) -> Self {
        IntelHexError::Invalid {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for IntelHexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntelHexError::Invalid {
                line: Some(line),
                message,
            } => {
                write!(formatter, "line {line}: {message}")
            }
            IntelHexError::Invalid {
                line: None,
                message,
            } => formatter.write_str(message),
            IntelHexError::OutOfMemory { size } => {
                write!(formatter, "there is not enough memory for 0x{size:x} bytes")
            }
        }
    }
}

/// Whether `path` names an Intel HEX file: it ends with `.hex` or `.ihex`, ASCII case-insensitive,
/// once cut at the first `?` or `#` (a URL).
///
/// The extension decides, not the content: a binary image may start with `:` (`0x3A`), and
/// Arduino, PlatformIO and `objcopy` always name Intel HEX `.hex`.
pub fn is_intel_hex(path: &str) -> bool {
    let end = path.find(['?', '#']).unwrap_or(path.len());
    ends_with_ignore_case(&path[..end], ".hex") || ends_with_ignore_case(&path[..end], ".ihex")
}

/// Whether `path` names an EEPROM image (`.eep`), which is Intel HEX nanoflash never writes.
pub(crate) fn is_eeprom_image(path: &str) -> bool {
    let end = path.find(['?', '#']).unwrap_or(path.len());
    ends_with_ignore_case(&path[..end], ".eep")
}

fn ends_with_ignore_case(text: &str, suffix: &str) -> bool {
    let (text, suffix) = (text.as_bytes(), suffix.as_bytes());
    text.len() >= suffix.len() && text[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// The most bytes a record holds: its length, address, type and checksum, and 255 data bytes.
const MAXIMUM_RECORD: usize = 5 + 255;

/// The span of the addresses of a record from its base address: a record may not cross it.
const SEGMENT: u64 = 0x1_0000;

/// One data record: where its bytes are in the pool.
struct Record {
    line: usize,
    address: u64,
    pool_start: usize,
    length: usize,
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The bytes a record line holds, checksum included, appended to `record`.
fn record_bytes(line: usize, text: &[u8], record: &mut Vec<u8>) -> Result<(), IntelHexError> {
    let Some(digits) = text.strip_prefix(b":") else {
        return Err(IntelHexError::invalid(
            Some(line),
            "a record starts with ':'",
        ));
    };
    if digits.len() % 2 != 0 {
        return Err(IntelHexError::invalid(
            Some(line),
            "a record has an odd number of hexadecimal digits",
        ));
    }
    if digits.len() / 2 > MAXIMUM_RECORD {
        return Err(IntelHexError::invalid(
            Some(line),
            format!(
                "a record has {} bytes, more than the {MAXIMUM_RECORD} of a record of 255 data bytes",
                digits.len() / 2
            ),
        ));
    }
    record.clear();
    record
        .try_reserve(digits.len() / 2)
        .map_err(|_| IntelHexError::OutOfMemory {
            size: MAXIMUM_RECORD as u64,
        })?;
    for pair in digits.chunks_exact(2) {
        let [high, low] = pair else { continue };
        match (digit(*high), digit(*low)) {
            (Some(high), Some(low)) => record.push(high << 4 | low),
            _ => {
                return Err(IntelHexError::invalid(
                    Some(line),
                    "a record has a character that is not a hexadecimal digit",
                ));
            }
        }
    }
    if record.len() < 5 {
        return Err(IntelHexError::invalid(
            Some(line),
            "a record has at least 5 bytes: length, address, type and checksum",
        ));
    }
    let length = usize::from(record[0]);
    if record.len() != length + 5 {
        return Err(IntelHexError::invalid(
            Some(line),
            format!(
                "the record says {length} data bytes and has {}",
                record.len() - 5
            ),
        ));
    }
    if record.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
        return Err(IntelHexError::invalid(Some(line), "the checksum is wrong"));
    }
    Ok(())
}

/// Decodes Intel HEX `text` into its flat image.
///
/// Lines are separated by `\n`, a trailing `\r` is dropped and blank lines are skipped. Data
/// records (`00`) are placed at the base address plus their own; the end record (`01`) must be
/// present and last; extended segment (`02`) and linear (`04`) addresses set the base; start
/// addresses (`03`, `05`) are ignored; other types, records covering the same byte, a file
/// without data, and data records that cross the 64 KiB boundary above their base address
/// (where the specification wraps them to the base), are errors.
///
/// `maximum_span` bounds the number of bytes from the lowest to the highest address, both
/// inclusive: the check is made before the image is allocated. Every allocation is fallible:
/// [`IntelHexError::OutOfMemory`] when one fails.
pub fn decode_intel_hex(text: &[u8], maximum_span: u64) -> Result<IntelHexImage, IntelHexError> {
    let mut pool: Vec<u8> = Vec::new();
    let mut records: Vec<Record> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut base = 0u64;
    let mut ended = false;
    for (index, raw) in text.split(|byte| *byte == b'\n').enumerate() {
        let line = index + 1;
        let content = raw.strip_suffix(b"\r").unwrap_or(raw);
        if content.is_empty() {
            continue;
        }
        if ended {
            return Err(IntelHexError::invalid(
                Some(line),
                "there is a record after the end record",
            ));
        }
        record_bytes(line, content, &mut scratch)?;
        let length = usize::from(scratch[0]);
        let address = u64::from(scratch[1]) << 8 | u64::from(scratch[2]);
        let data = &scratch[4..4 + length];
        let need = |expected: usize, name: &str| {
            if length == expected {
                Ok(())
            } else {
                Err(IntelHexError::invalid(
                    Some(line),
                    format!("the {name} record has {length} data bytes instead of {expected}"),
                ))
            }
        };
        match scratch[3] {
            0x00 => {
                if address + length as u64 > SEGMENT {
                    return Err(IntelHexError::invalid(
                        Some(line),
                        format!(
                            "the record of {length} bytes at 0x{address:04x} crosses the 64 KiB boundary at 0x{:x}, where the specification wraps it to 0x{base:x}: split it there and set the next base address with an extended address record",
                            base + SEGMENT
                        ),
                    ));
                }
                if length > 0 {
                    let out_of_memory = |size: usize| IntelHexError::OutOfMemory {
                        size: u64::try_from(size).unwrap_or(u64::MAX),
                    };
                    records.try_reserve(1).map_err(|_| {
                        out_of_memory(
                            records
                                .len()
                                .saturating_add(1)
                                .saturating_mul(core::mem::size_of::<Record>()),
                        )
                    })?;
                    pool.try_reserve(length)
                        .map_err(|_| out_of_memory(pool.len().saturating_add(length)))?;
                    records.push(Record {
                        line,
                        address: base + address,
                        pool_start: pool.len(),
                        length,
                    });
                    pool.extend_from_slice(data);
                }
            }
            0x01 => {
                need(0, "end")?;
                ended = true;
            }
            0x02 => {
                need(2, "extended segment address")?;
                base = (u64::from(data[0]) << 8 | u64::from(data[1])) * 16;
            }
            0x04 => {
                need(2, "extended linear address")?;
                base = (u64::from(data[0]) << 8 | u64::from(data[1])) << 16;
            }
            0x03 => need(4, "start segment address")?,
            0x05 => need(4, "start linear address")?,
            other => {
                return Err(IntelHexError::invalid(
                    Some(line),
                    format!("the record type 0x{other:02x} is unknown"),
                ));
            }
        }
    }
    if !ended {
        return Err(IntelHexError::invalid(None, "there is no end record"));
    }
    if records.is_empty() {
        return Err(IntelHexError::invalid(None, "there is no data record"));
    }

    // In place: a stable sort would allocate. Lines are unique, so the order is the same.
    records.sort_unstable_by_key(|record| (record.address, record.line));
    // The highest address so far, exclusive, and the line of the record that reaches it.
    let mut reach: Option<(u64, usize)> = None;
    for record in &records {
        if let Some((end, line)) = reach {
            if record.address < end {
                return Err(IntelHexError::invalid(
                    Some(record.line.max(line)),
                    format!(
                        "the records of lines {} and {} both cover the byte at 0x{:x}",
                        record.line.min(line),
                        record.line.max(line),
                        record.address
                    ),
                ));
            }
        }
        let end = record.address + record.length as u64;
        reach = Some((end, record.line));
    }
    let lowest = records[0].address;
    let highest = reach.map_or(lowest, |(end, _)| end - 1);
    let span = highest - lowest + 1;
    if span > maximum_span {
        return Err(IntelHexError::invalid(
            None,
            format!(
                "spans 0x{span:x} bytes from 0x{lowest:x} to 0x{highest:x}, more than the 0x{maximum_span:x} bytes of flash"
            ),
        ));
    }
    let out_of_memory = IntelHexError::OutOfMemory { size: span };
    let size = usize::try_from(span).map_err(|_| out_of_memory.clone())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| out_of_memory.clone())?;
    bytes.resize(size, 0xFF);
    for record in &records {
        let offset = usize::try_from(record.address - lowest).map_err(|_| out_of_memory.clone())?;
        if let (Some(target), Some(source)) = (
            bytes.get_mut(offset..offset + record.length),
            pool.get(record.pool_start..record.pool_start + record.length),
        ) {
            target.copy_from_slice(source);
        }
    }
    Ok(IntelHexImage {
        start: lowest,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    const FIXTURE: &str = include_str!("../../tests/images/avr-application.hex");

    fn invalid_line(text: &str) -> Option<usize> {
        match decode_intel_hex(text.as_bytes(), 0x1_0000_0000) {
            Err(IntelHexError::Invalid { line, .. }) => line,
            other => panic!("expected an invalid image, got {other:?}"),
        }
    }

    #[test]
    fn decodes_the_fixture() {
        let image = decode_intel_hex(FIXTURE.as_bytes(), 0x8000).unwrap();
        assert_eq!(image.start, 0);
        assert_eq!(image.bytes.len(), 300);
        assert_eq!(image.bytes[299], ((299 * 7 + 1) % 256) as u8);
        let windows = FIXTURE.replace('\n', "\r\n").to_lowercase();
        assert_eq!(decode_intel_hex(windows.as_bytes(), 0x8000).unwrap(), image);
    }

    #[test]
    fn follows_extended_addresses() {
        let segment = ":020000021000EC\n:0100000055AA\n:00000001FF\n";
        let image = decode_intel_hex(segment.as_bytes(), 16).unwrap();
        assert_eq!((image.start, image.bytes), (0x10000, alloc::vec![0x55]));
        let linear = ":020000040800F2\n:0400000001020304F2\n:00000001FF\n";
        let image = decode_intel_hex(linear.as_bytes(), 16).unwrap();
        assert_eq!(image.start, 0x0800_0000);
        assert_eq!(image.bytes, [1, 2, 3, 4]);
        let starts = ":0400000300007E007B\n:04000005000000CD2A\n:0100000055AA\n:00000001FF\n";
        assert_eq!(
            decode_intel_hex(starts.as_bytes(), 16).unwrap().bytes,
            [0x55]
        );
    }

    #[test]
    fn fills_gaps_and_accepts_any_order() {
        let text = ":0100100011DE\n:0100000022DD\n:00000001FF\n";
        let image = decode_intel_hex(text.as_bytes(), 0x20).unwrap();
        assert_eq!(image.start, 0);
        assert_eq!(image.bytes.len(), 17);
        assert_eq!((image.bytes[0], image.bytes[16]), (0x22, 0x11));
        assert!(image.bytes[1..16].iter().all(|byte| *byte == 0xFF));
    }

    #[test]
    fn names_the_line_of_each_error() {
        assert_eq!(
            invalid_line(":0100000055AA\n:0100000055AB\n:00000001FF\n"),
            Some(2)
        );
        assert_eq!(invalid_line("0100000055AA\n:00000001FF\n"), Some(1));
        assert_eq!(invalid_line(":0100000055A\n:00000001FF\n"), Some(1));
        assert_eq!(invalid_line(":0200000055AA\n:00000001FF\n"), Some(1));
        assert_eq!(invalid_line(":0100000655A4\n:00000001FF\n"), Some(1));
        assert_eq!(
            invalid_line(":0100080011E6\n\n:020008002233A1\n:00000001FF\n"),
            Some(3)
        );
        assert_eq!(
            invalid_line(":0100000055AA\n:00000001FF\n:0100000055AA\n"),
            Some(3)
        );
        assert_eq!(invalid_line(":0100000055AA\n"), None);
        assert_eq!(invalid_line(":00000001FF\n"), None);
    }

    #[test]
    fn refuses_spans_larger_than_the_flash() {
        let text = ":0100000055AA\n:020000040000FA\n:02000004FFFFFC\n:0100000011EE\n:00000001FF\n";
        let error = decode_intel_hex(text.as_bytes(), 0x8000).unwrap_err();
        assert!(error.to_string().contains("0xffff0000"), "{error}");
        assert!(error.to_string().contains("0x8000"), "{error}");
        // A span of exactly the maximum passes.
        let exact = ":0100000055AA\n:01001F0011CF\n:00000001FF\n";
        assert!(decode_intel_hex(exact.as_bytes(), 0x20).is_ok());
        assert!(decode_intel_hex(exact.as_bytes(), 0x1F).is_err());
    }

    #[test]
    fn refuses_records_that_cross_64_kib() {
        // Two bytes at 0xFFFF: the second would be at 0x10000, which the specification wraps to
        // 0x0.
        let crossing = ":02FFFF001122CD\n:00000001FF\n";
        let error = decode_intel_hex(crossing.as_bytes(), 0x2_0000).unwrap_err();
        assert_eq!(invalid_line(crossing), Some(1));
        let text = error.to_string();
        assert!(
            text.contains("0xffff") && text.contains("0x10000"),
            "{text}"
        );
        // Above an extended linear address, the boundary moves with it.
        let linear = ":020000040001F9\n:02FFFF001122CD\n:00000001FF\n";
        let text = decode_intel_hex(linear.as_bytes(), 0x2_0000)
            .unwrap_err()
            .to_string();
        assert!(
            text.contains("0x20000") && text.contains("0x10000"),
            "{text}"
        );
        // Ending exactly at the boundary is fine.
        let last = ":01FFFF0011F0\n:00000001FF\n";
        let image = decode_intel_hex(last.as_bytes(), 0x2_0000).unwrap();
        assert_eq!((image.start, image.bytes), (0xFFFF, alloc::vec![0x11]));
    }

    #[test]
    fn refuses_records_longer_than_255_data_bytes() {
        let digits = "00".repeat(300);
        let text = alloc::format!(":{digits}\n:00000001FF\n");
        let error = decode_intel_hex(text.as_bytes(), 0x1000).unwrap_err();
        assert_eq!(invalid_line(&text), Some(1));
        assert!(error.to_string().contains("300 bytes"), "{error}");
    }

    #[test]
    fn agrees_with_the_ihex_crate() {
        let mut expected = Vec::new();
        for record in ihex::Reader::new(FIXTURE) {
            if let ihex::Record::Data { value, .. } = record.unwrap() {
                expected.extend_from_slice(&value);
            }
        }
        let image = decode_intel_hex(FIXTURE.as_bytes(), 0x8000).unwrap();
        assert_eq!(image.bytes, expected);
    }

    #[test]
    fn extension_decides() {
        for path in ["a.hex", "A.HEX", "x/a.ihex?v=2", "https://h/a.hex#top"] {
            assert!(is_intel_hex(path), "{path}");
        }
        for path in ["a.bin", "hex", "a.hex.bin", "a.eep"] {
            assert!(!is_intel_hex(path), "{path}");
        }
        assert!(is_eeprom_image("sketch.EEP"));
        assert!(!is_eeprom_image("sketch.hex"));
    }
}
