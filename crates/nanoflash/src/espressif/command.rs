//! Encoding of ROM loader requests and parsing of its responses.

use alloc::vec::Vec;

/// The command opcodes the backend uses.
pub(crate) mod opcode {
    pub const FLASH_BEGIN: u8 = 0x02;
    pub const FLASH_END: u8 = 0x04;
    pub const SYNC: u8 = 0x08;
    pub const WRITE_REG: u8 = 0x09;
    pub const READ_REG: u8 = 0x0A;
    pub const SPI_SET_PARAMS: u8 = 0x0B;
    pub const SPI_ATTACH: u8 = 0x0D;
    pub const CHANGE_BAUDRATE: u8 = 0x0F;
    pub const FLASH_DEFL_BEGIN: u8 = 0x10;
    pub const FLASH_DEFL_DATA: u8 = 0x11;
    pub const SPI_FLASH_MD5: u8 = 0x13;
    pub const GET_SECURITY_INFO: u8 = 0x14;
}

/// The name of an opcode, for messages.
pub(crate) fn name(opcode: u8) -> &'static str {
    match opcode {
        opcode::FLASH_BEGIN => "FLASH_BEGIN",
        opcode::FLASH_END => "FLASH_END",
        opcode::SYNC => "SYNC",
        opcode::WRITE_REG => "WRITE_REG",
        opcode::READ_REG => "READ_REG",
        opcode::SPI_SET_PARAMS => "SPI_SET_PARAMS",
        opcode::SPI_ATTACH => "SPI_ATTACH",
        opcode::CHANGE_BAUDRATE => "CHANGE_BAUDRATE",
        opcode::FLASH_DEFL_BEGIN => "FLASH_DEFL_BEGIN",
        opcode::FLASH_DEFL_DATA => "FLASH_DEFL_DATA",
        opcode::SPI_FLASH_MD5 => "SPI_FLASH_MD5",
        opcode::GET_SECURITY_INFO => "GET_SECURITY_INFO",
        _ => "unknown command",
    }
}

/// The payload of `SYNC`: `07 07 12 20` then 32 times `55`.
pub(crate) const SYNC_DATA: [u8; 36] = {
    let mut data = [0x55; 36];
    data[0] = 0x07;
    data[1] = 0x07;
    data[2] = 0x12;
    data[3] = 0x20;
    data
};

/// A request packet: direction `0x00`, opcode, size, checksum and data.
pub(crate) fn request(opcode: u8, data: &[u8], checksum: u32) -> Vec<u8> {
    let size = u16::try_from(data.len()).unwrap_or(u16::MAX);
    let mut packet = Vec::with_capacity(data.len().saturating_add(8));
    packet.push(0x00);
    packet.push(opcode);
    packet.extend_from_slice(&size.to_le_bytes());
    packet.extend_from_slice(&checksum.to_le_bytes());
    packet.extend_from_slice(data);
    packet
}

/// Little-endian 32-bit words.
pub(crate) fn words(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len().saturating_mul(4));
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// The payload of `FLASH_BEGIN` and `FLASH_DEFL_BEGIN`; the fifth word is the ROM loader's
/// encryption flag, absent on the ESP32.
pub(crate) fn begin_data(
    size: u32,
    blocks: u32,
    block_size: u32,
    offset: u32,
    encryption_word: bool,
) -> Vec<u8> {
    if encryption_word {
        words(&[size, blocks, block_size, offset, 0])
    } else {
        words(&[size, blocks, block_size, offset])
    }
}

/// The payload of a data block and its checksum (XOR of the data seeded with `0xEF`).
pub(crate) fn block(data: &[u8], sequence: u32) -> (Vec<u8>, u32) {
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let mut payload = words(&[length, sequence, 0, 0]);
    payload.extend_from_slice(data);
    let checksum = data.iter().fold(0xEF_u8, |sum, byte| sum ^ byte);
    (payload, u32::from(checksum))
}

/// A parsed ROM loader response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Response {
    pub opcode: u8,
    pub value: u32,
    pub data: Vec<u8>,
    pub status: u8,
    pub error: u8,
}

/// Parses a decoded frame; `None` when it is not a well-formed response.
pub(crate) fn parse_response(frame: &[u8]) -> Option<Response> {
    if frame.len() < 12 || *frame.first()? != 0x01 {
        return None;
    }
    let size = usize::from(u16::from_le_bytes([*frame.get(2)?, *frame.get(3)?]));
    if size != frame.len().checked_sub(8)? {
        return None;
    }
    let value = u32::from_le_bytes([
        *frame.get(4)?,
        *frame.get(5)?,
        *frame.get(6)?,
        *frame.get(7)?,
    ]);
    let body = frame.get(8..)?;
    let split = body.len().checked_sub(4)?;
    Some(Response {
        opcode: *frame.get(1)?,
        value,
        data: body.get(..split)?.to_vec(),
        status: *body.get(split)?,
        error: *body.get(split.checked_add(1)?)?,
    })
}

/// Parses the 32 hexadecimal digits of an `SPI_FLASH_MD5` answer.
pub(crate) fn parse_md5(data: &[u8]) -> Option<[u8; 16]> {
    if data.len() != 32 {
        return None;
    }
    let mut digest = [0_u8; 16];
    for (slot, pair) in digest.iter_mut().zip(data.chunks_exact(2)) {
        let high = digit(*pair.first()?)?;
        let low = digit(*pair.get(1)?)?;
        *slot = high << 4 | low;
    }
    Some(digest)
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
