//! Sizes and offsets written as integers, hexadecimal or numbers with a binary unit.

use alloc::string::{String, ToString};
use core::fmt;

/// A text that is not a valid size or offset.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuantityError {
    /// The rejected text.
    pub text: String,
}

impl fmt::Display for QuantityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid size or offset \"{}\": expected an integer (4096), hexadecimal (0x1000) or a number with a unit (4 KiB, 8 MiB, 1 GiB, 512 B)",
            self.text
        )
    }
}

impl core::error::Error for QuantityError {}

/// Parse `4096`, `0x1000`, `4 KiB`, `8MiB`, `1 GiB` or `512 B` into a number of bytes.
pub fn parse(text: &str) -> Result<u64, QuantityError> {
    parse_inner(text).ok_or_else(|| QuantityError {
        text: text.to_string(),
    })
}

fn parse_inner(text: &str) -> Option<u64> {
    let text = text.trim_matches(' ');
    if let Some(digits) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        return u64::from_str_radix(digits, 16).ok();
    }
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    if digits.is_empty() {
        return None;
    }
    let number: u64 = digits.parse().ok()?;
    let multiplier: u64 = match unit.trim_start_matches(' ') {
        "" if unit.is_empty() => 1,
        "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

/// Write a number of bytes with the largest binary unit that divides it exactly.
pub fn format(bytes: u64) -> String {
    for (unit, shift) in [("GiB", 30), ("MiB", 20), ("KiB", 10)] {
        let size = 1u64 << shift;
        if bytes != 0 && bytes % size == 0 {
            return alloc::format!("{} {unit}", bytes / size);
        }
    }
    alloc::format!("{bytes} B")
}

/// Deserialise a size or offset written as an integer or as text [`parse`] accepts.
pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    use serde::de::{Error, Visitor};

    struct QuantityVisitor;

    impl Visitor<'_> for QuantityVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an integer, hexadecimal or a number with a unit")
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }

        fn visit_i64<E: Error>(self, value: i64) -> Result<u64, E> {
            u64::try_from(value).map_err(|_| E::custom(alloc::format!("negative number {value}")))
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<u64, E> {
            parse(value).map_err(E::custom)
        }
    }

    deserializer.deserialize_any(QuantityVisitor)
}

/// [`deserialize`] for an optional key.
pub(crate) fn deserialize_optional<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_accepted_forms() {
        assert_eq!(parse("4096"), Ok(4096));
        assert_eq!(parse("0x1000"), Ok(0x1000));
        assert_eq!(parse("0X1000"), Ok(0x1000));
        assert_eq!(parse("4 KiB"), Ok(4096));
        assert_eq!(parse(" 8MiB "), Ok(8 << 20));
        assert_eq!(parse("512 B"), Ok(512));
    }

    #[test]
    fn rejects_near_misses() {
        for text in [
            "4KB",
            "4 kib",
            "4k",
            "4 K",
            "-1",
            "1.5 MiB",
            "",
            "0x",
            "99999999999 GiB",
            "+4",
            "0x+1",
            "4\tKiB",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert!(parse("4KB").unwrap_err().to_string().contains("4 KiB"));
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format(32 * 1024), "32 KiB");
        assert_eq!(format(8 << 20), "8 MiB");
        assert_eq!(format(1000), "1000 B");
        assert_eq!(format(0), "0 B");
    }
}
