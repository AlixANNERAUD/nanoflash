// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! The STK500v2 messages of the wiring bootloader of the Mega 2560.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The start of every message.
pub(crate) const START: u8 = 0x1B;
/// The bytes before the body: start, sequence, body length and token.
pub(crate) const HEADER_LENGTH: usize = 5;
/// The longest body an answer may have, twice a page of the ATmega2560: a longer announced
/// length is noise.
pub(crate) const MAXIMUM_BODY: usize = 512;
/// The token before the body.
const TOKEN: u8 = 0x0E;
/// `STATUS_CMD_OK`.
const STATUS_OK: u8 = 0x00;

/// `CMD_SIGN_ON`.
pub(crate) const SIGN_ON: [u8; 1] = [0x01];
/// `CMD_LEAVE_PROGMODE_ISP`.
pub(crate) const LEAVE: [u8; 3] = [0x11, 0x01, 0x01];

/// A message: start, sequence, body length, token, body and the XOR of every byte before the
/// checksum. A body over 65535 bytes saturates its length; bodies are a page long.
pub(crate) fn message(sequence: u8, body: &[u8]) -> Vec<u8> {
    let length = u16::try_from(body.len()).unwrap_or(u16::MAX);
    let [high, low] = length.to_be_bytes();
    let mut message = Vec::with_capacity(body.len().saturating_add(6));
    message.extend_from_slice(&[START, sequence, high, low, TOKEN]);
    message.extend_from_slice(body);
    message.push(checksum(&message));
    message
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum ^ byte)
}

/// The data of the answer to `command`, from a whole message: the framing, the sequence number
/// and the checksum are checked, the body must start with the command and `STATUS_CMD_OK`.
pub(crate) fn answer(message: &[u8], sequence: u8, command: u8) -> Result<&[u8], String> {
    let [start, received, high, low, token, rest @ ..] = message else {
        return Err(format!("the message is {} bytes, too short", message.len()));
    };
    if *start != START {
        return Err(format!("the message starts with {start:02x}, not 1b"));
    }
    if *received != sequence {
        return Err(format!(
            "the sequence number is {received}, expected {sequence}"
        ));
    }
    if *token != TOKEN {
        return Err(format!("the token is {token:02x}, not 0e"));
    }
    let length = usize::from(u16::from_be_bytes([*high, *low]));
    let Some((body, [sum])) = (length <= rest.len())
        .then(|| rest.split_at(length))
        .filter(|(_, tail)| tail.len() == 1)
    else {
        return Err(format!(
            "the body length is {length}, but {} bytes follow the header",
            rest.len()
        ));
    };
    let expected = checksum(&message[..message.len() - 1]);
    if *sum != expected {
        return Err(format!(
            "the checksum is {sum:02x}, expected {expected:02x}"
        ));
    }
    match body {
        [received, status, data @ ..] if *received == command => {
            if *status == STATUS_OK {
                Ok(data)
            } else {
                Err(format!(
                    "the status of command {command:02x} is {status:02x}"
                ))
            }
        }
        [received, ..] => Err(format!(
            "the answer is to command {received:02x}, expected {command:02x}"
        )),
        [] => Err(String::from("the answer has no body")),
    }
}

/// The length of the whole message that starts with `header`: the header, the body and the
/// checksum; `None` when its body is longer than [`MAXIMUM_BODY`].
pub(crate) fn message_length(header: [u8; HEADER_LENGTH]) -> Option<usize> {
    let [_, _, high, low, _] = header;
    let body = usize::from(u16::from_be_bytes([high, low]));
    (body <= MAXIMUM_BODY).then_some(HEADER_LENGTH + body + 1)
}

/// The data of an answer whose body ends with a second `STATUS_CMD_OK`, as those of
/// `CMD_READ_SIGNATURE_ISP` and `CMD_READ_FLASH_ISP`: [`answer`], then that status checked and
/// stripped.
pub(crate) fn answer_with_status(
    message: &[u8],
    sequence: u8,
    command: u8,
) -> Result<&[u8], String> {
    match answer(message, sequence, command)? {
        [data @ .., STATUS_OK] => Ok(data),
        [.., status] => Err(format!(
            "the status after the data of command {command:02x} is {status:02x}"
        )),
        [] => Err(format!(
            "the answer to command {command:02x} has no status after the data"
        )),
    }
}

/// `CMD_READ_SIGNATURE_ISP` for signature byte `index` (0 to 2).
pub(crate) fn read_signature(index: u8) -> [u8; 6] {
    [0x1B, 0x04, 0x30, 0x00, index, 0x00]
}

/// `CMD_LOAD_ADDRESS` for a byte offset, taken as a word address with bit 31 set (the extended
/// address); `None` when the word needs more than 31 bits.
pub(crate) fn load_address(offset: u64) -> Option<[u8; 5]> {
    let word = u32::try_from(offset / 2)
        .ok()
        .filter(|word| *word < 1 << 31)?;
    let [a, b, c, d] = (word | 1 << 31).to_be_bytes();
    Some([0x06, a, b, c, d])
}

/// `CMD_PROGRAM_FLASH_ISP` in AVR068 mode `C1` (page mode, write page, ready/busy polling; the
/// bootloader ignores the mode, delay and instruction bytes). The length saturates at 65535.
pub(crate) fn program_flash(data: &[u8]) -> Vec<u8> {
    let length = u16::try_from(data.len()).unwrap_or(u16::MAX);
    let [high, low] = length.to_be_bytes();
    let mut body = Vec::with_capacity(data.len().saturating_add(10));
    body.extend_from_slice(&[0x13, high, low, 0xC1, 0x0A, 0x40, 0x4C, 0x20, 0x00, 0x00]);
    body.extend_from_slice(data);
    body
}

/// `CMD_READ_FLASH_ISP`.
pub(crate) fn read_flash(length: u16) -> [u8; 4] {
    let [high, low] = length.to_be_bytes();
    [0x14, high, low, 0x20]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGN_ON_ANSWER: [u8; 16] = [
        0x1b, 0x01, 0x00, 0x0b, 0x0e, 0x01, 0x00, 0x08, b'A', b'V', b'R', b'I', b'S', b'P', b'_',
        b'2',
    ];

    fn sign_on_answer() -> Vec<u8> {
        let mut bytes = SIGN_ON_ANSWER.to_vec();
        bytes.push(0x74);
        bytes
    }

    #[test]
    fn stk500v2_framing_is_checked() {
        assert_eq!(
            message(1, &[0x01]),
            [0x1b, 0x01, 0x00, 0x01, 0x0e, 0x01, 0x14]
        );
        assert_eq!(
            message(2, &read_signature(0)),
            [
                0x1b, 0x02, 0x00, 0x06, 0x0e, 0x1b, 0x04, 0x30, 0x00, 0x00, 0x00, 0x3e
            ]
        );
        assert_eq!(
            message(5, &load_address(0).unwrap()),
            [
                0x1b, 0x05, 0x00, 0x05, 0x0e, 0x06, 0x80, 0x00, 0x00, 0x00, 0x93
            ]
        );
        assert_eq!(
            message(7, &LEAVE),
            [0x1b, 0x07, 0x00, 0x03, 0x0e, 0x11, 0x01, 0x01, 0x00]
        );
        let good = sign_on_answer();
        assert_eq!(answer(&good, 1, 0x01), Ok(&b"\x08AVRISP_2"[..]));

        let mut bad = good.clone();
        bad[16] = 0x75;
        assert!(answer(&bad, 1, 0x01).unwrap_err().contains("checksum"));
        assert!(answer(&good, 2, 0x01).unwrap_err().contains("sequence"));
        let mut bad = good.clone();
        bad[4] = 0x0F;
        assert!(answer(&bad, 1, 0x01).unwrap_err().contains("token"));
        let mut bad = good.clone();
        bad[6] = 0xC0;
        bad[16] = checksum(&bad[..16]);
        assert!(answer(&bad, 1, 0x01).unwrap_err().contains("status"));
        assert!(answer(&good, 1, 0x13).unwrap_err().contains("command"));
        let mut bad = good.clone();
        bad[0] = 0x00;
        assert!(answer(&bad, 1, 0x01).unwrap_err().contains("starts"));
        assert!(answer(&good[..10], 1, 0x01).is_err());
        assert!(answer(&good[..16], 1, 0x01).is_err());
        assert!(answer(&[], 1, 0x01).is_err());
        assert!(answer(&[0x1b, 0x01, 0xFF, 0xFF, 0x0e, 0x01], 1, 0x01).is_err());

        assert_eq!(load_address(0x100), Some([0x06, 0x80, 0x00, 0x00, 0x80]));
        assert_eq!(load_address(1 << 32), None);
        assert_eq!(read_signature(2), [0x1B, 0x04, 0x30, 0x00, 0x02, 0x00]);
    }

    #[test]
    fn reads_end_with_a_second_status() {
        let signature = [0x1b, 0x03, 0x00, 0x04, 0x0e, 0x1b, 0x00, 0x98, 0x00, 0x91];
        assert_eq!(answer_with_status(&signature, 3, 0x1B), Ok(&[0x98][..]));
        let mut failed = signature;
        failed[8] = 0xC0;
        failed[9] = checksum(&failed[..9]);
        let message = answer_with_status(&failed, 3, 0x1B).unwrap_err();
        assert!(message.contains("c0"), "{message}");
        let empty = message_without_data();
        assert!(answer_with_status(&empty, 1, 0x1B).is_err());
        assert_eq!(message_length([0x1b, 0x01, 0x00, 0x0b, 0x0e]), Some(17));
        assert_eq!(message_length([0x1b, 0x01, 0x02, 0x00, 0x0e]), Some(518));
        assert_eq!(message_length([0x1b, 0x01, 0x02, 0x01, 0x0e]), None);
    }

    /// An answer to `CMD_READ_SIGNATURE_ISP` with the command and the status only.
    fn message_without_data() -> Vec<u8> {
        let mut bytes = alloc::vec![0x1b, 0x01, 0x00, 0x02, 0x0e, 0x1b, 0x00];
        bytes.push(checksum(&bytes));
        bytes
    }

    #[test]
    fn stk500v2_flash_commands() {
        let program = program_flash(&[0xAA; 256]);
        assert_eq!(
            program[..10],
            [0x13, 0x01, 0x00, 0xC1, 0x0A, 0x40, 0x4C, 0x20, 0x00, 0x00]
        );
        assert_eq!(program.len(), 266);
        assert_eq!(read_flash(256), [0x14, 0x01, 0x00, 0x20]);
        assert_eq!(SIGN_ON, [0x01]);
    }
}
