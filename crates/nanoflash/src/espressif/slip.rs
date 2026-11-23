//! SLIP framing (RFC 1055 flavour used by the ROM loader).

use alloc::vec::Vec;

const END: u8 = 0xC0;
const ESCAPE: u8 = 0xDB;
const ESCAPED_END: u8 = 0xDC;
const ESCAPED_ESCAPE: u8 = 0xDD;

/// The largest frame the decoder keeps; longer frames are dropped.
const MAXIMUM_FRAME: usize = 64 * 1024;
/// The most noise bytes the decoder remembers.
const MAXIMUM_NOISE: usize = 4 * 1024;

/// Frames a packet: `0xC0`, the escaped payload, `0xC0`.
pub(crate) fn encode(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.len().saturating_add(2));
    out.push(END);
    for &byte in packet {
        match byte {
            END => out.extend_from_slice(&[ESCAPE, ESCAPED_END]),
            ESCAPE => out.extend_from_slice(&[ESCAPE, ESCAPED_ESCAPE]),
            other => out.push(other),
        }
    }
    out.push(END);
    out
}

/// Splits a byte stream into frames, keeping the bytes seen outside frames (boot text).
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    in_frame: bool,
    escaped: bool,
    overflowed: bool,
    frame: Vec<u8>,
    noise: Vec<u8>,
}

impl Decoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feeds one byte; returns a frame when this byte completes one.
    pub(crate) fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        if !self.in_frame {
            if byte == END {
                self.in_frame = true;
                self.escaped = false;
                self.overflowed = false;
                self.frame.clear();
            } else {
                self.push_noise(byte);
            }
            return None;
        }
        if byte == END {
            // An empty frame is two adjacent delimiters: the second one starts the real frame.
            if self.frame.is_empty() && !self.overflowed {
                return None;
            }
            self.in_frame = false;
            self.escaped = false;
            if self.overflowed {
                self.overflowed = false;
                self.frame.clear();
                return None;
            }
            return Some(core::mem::take(&mut self.frame));
        }
        let decoded = if self.escaped {
            self.escaped = false;
            match byte {
                ESCAPED_END => END,
                ESCAPED_ESCAPE => ESCAPE,
                other => other,
            }
        } else if byte == ESCAPE {
            self.escaped = true;
            return None;
        } else {
            byte
        };
        if self.frame.len() >= MAXIMUM_FRAME {
            self.overflowed = true;
            self.frame.clear();
        } else if !self.overflowed {
            self.frame.push(decoded);
        }
        None
    }

    /// The bytes seen outside frames since the last call, at most the last 4 KiB.
    pub(crate) fn take_noise(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.noise)
    }

    fn push_noise(&mut self, byte: u8) {
        if self.noise.len() >= MAXIMUM_NOISE {
            self.noise.remove(0);
        }
        self.noise.push(byte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_both_special_bytes() {
        assert_eq!(
            encode(&[0xC0, 0xDB, 0x01]),
            [0xC0, 0xDB, 0xDC, 0xDB, 0xDD, 0x01, 0xC0]
        );
    }

    #[test]
    fn empty_frames_are_ignored() {
        let mut decoder = Decoder::new();
        let mut frames = Vec::new();
        for byte in [0xC0, 0xC0, 0xC0, 0x01, 0xC0] {
            frames.extend(decoder.push(byte));
        }
        assert_eq!(frames, [[0x01]]);
    }

    #[test]
    fn oversized_frames_are_dropped() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(0xC0), None);
        for _ in 0..MAXIMUM_FRAME + 10 {
            assert_eq!(decoder.push(0x01), None);
        }
        assert_eq!(decoder.push(0xC0), None);
        assert_eq!(decoder.take_noise(), Vec::<u8>::new());
        decoder.push(0xC0);
        decoder.push(0x07);
        assert_eq!(decoder.push(0xC0), Some(alloc::vec![0x07]));
    }

    #[test]
    fn noise_keeps_the_last_four_kibibytes() {
        let mut decoder = Decoder::new();
        for index in 0..(MAXIMUM_NOISE + 5) {
            decoder.push((index % 100) as u8 + 1);
        }
        let noise = decoder.take_noise();
        assert_eq!(noise.len(), MAXIMUM_NOISE);
        assert_eq!(noise[0], 6);
        assert!(decoder.take_noise().is_empty());
    }
}
