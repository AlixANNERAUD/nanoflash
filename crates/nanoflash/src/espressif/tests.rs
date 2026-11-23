use super::Chip;
use super::command::*;
use super::slip;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

fn frame(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|part| u8::from_str_radix(part, 16).unwrap())
        .collect()
}

fn encoded(opcode: u8, data: &[u8]) -> String {
    hex(&slip::encode(&request(opcode, data, 0)))
}

#[test]
fn encodes_sync() {
    assert_eq!(
        hex(&slip::encode(&request(opcode::SYNC, &SYNC_DATA, 0))),
        "c0 00 08 24 00 00 00 00 00 07 07 12 20 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 55 c0"
    );
}

#[test]
fn encodes_requests() {
    assert_eq!(
        encoded(opcode::READ_REG, &words(&[0x4000_1000])),
        "c0 00 0a 04 00 00 00 00 00 00 10 00 40 c0"
    );
    assert_eq!(
        encoded(
            opcode::WRITE_REG,
            &words(&[0x6000_80B0, super::chip::WATCHDOG_KEY, 0xFFFF_FFFF, 0])
        ),
        "c0 00 09 10 00 00 00 00 00 b0 80 00 60 a1 3a d8 50 ff ff ff ff 00 00 00 00 c0"
    );
    assert_eq!(
        encoded(opcode::SPI_ATTACH, &words(&[0, 0])),
        "c0 00 0d 08 00 00 00 00 00 00 00 00 00 00 00 00 00 c0"
    );
    assert_eq!(
        encoded(
            opcode::SPI_SET_PARAMS,
            &words(&[0, 0x80_0000, 0x1_0000, 0x1000, 0x100, 0xFFFF])
        ),
        "c0 00 0b 18 00 00 00 00 00 00 00 00 00 00 00 80 00 00 00 01 00 00 10 00 00 00 01 00 00 ff ff 00 00 c0"
    );
    assert_eq!(
        encoded(opcode::CHANGE_BAUDRATE, &words(&[460_800, 0])),
        "c0 00 0f 08 00 00 00 00 00 00 08 07 00 00 00 00 00 c0"
    );
    assert_eq!(
        encoded(
            opcode::FLASH_DEFL_BEGIN,
            &begin_data(0x1000, 1, 0x400, 0, true)
        ),
        "c0 00 10 14 00 00 00 00 00 00 10 00 00 01 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 c0"
    );
    assert_eq!(
        encoded(
            opcode::FLASH_DEFL_BEGIN,
            &begin_data(0x1000, 1, 0x400, 0x1000, false)
        ),
        "c0 00 10 10 00 00 00 00 00 00 10 00 00 01 00 00 00 00 04 00 00 00 10 00 00 c0"
    );
    assert_eq!(
        encoded(opcode::SPI_FLASH_MD5, &words(&[0, 0x1000, 0, 0])),
        "c0 00 13 10 00 00 00 00 00 00 00 00 00 00 10 00 00 00 00 00 00 00 00 00 00 c0"
    );
    assert_eq!(
        encoded(opcode::GET_SECURITY_INFO, &[]),
        "c0 00 14 00 00 00 00 00 00 c0"
    );
}

#[test]
fn block_checksum_is_seeded_xor() {
    let (payload, checksum) = block(&[0x01, 0x02], 3);
    assert_eq!(checksum, 0xEC);
    assert_eq!(payload[..16], words(&[2, 3, 0, 0])[..]);
    assert_eq!(payload[16..], [0x01, 0x02]);
}

#[test]
fn parses_rom_responses() {
    let ok = parse_response(&frame("01 0a 04 00 83 1d f0 00 00 00 00 00")).unwrap();
    assert_eq!(
        (ok.opcode, ok.value, ok.status, ok.error),
        (0x0A, 0x00F0_1D83, 0, 0)
    );
    assert!(ok.data.is_empty());
    let failed = parse_response(&frame("01 14 04 00 00 00 00 00 01 05 00 00")).unwrap();
    assert_eq!((failed.status, failed.error), (1, 5));
    assert!(parse_response(&frame("00 0a 04 00 83 1d f0 00 00 00 00 00")).is_none());
    assert!(parse_response(&frame("01 0a 05 00 83 1d f0 00 00 00 00 00")).is_none());
    assert!(parse_response(&frame("01 0a 03 00 83 1d f0 00 00 00 00")).is_none());
}

#[test]
fn decoder_unescapes_and_keeps_noise() {
    let mut decoder = slip::Decoder::new();
    let mut frames = Vec::new();
    let mut stream = b"rst:0x1 boot:0x8\r\n".to_vec();
    stream.extend(frame("c0 01 0a 04 00 c8 db dc db dd 00 00 00 00 00 c0"));
    for byte in stream {
        frames.extend(decoder.push(byte));
    }
    assert_eq!(frames.len(), 1);
    assert_eq!(parse_response(&frames[0]).unwrap().value, 0x00DB_C0C8);
    assert_eq!(decoder.take_noise(), b"rst:0x1 boot:0x8\r\n");
}

#[test]
fn parses_md5() {
    let expected = [
        0xa0, 0xc1, 0x66, 0x16, 0xc9, 0x19, 0x07, 0xbd, 0x14, 0xe9, 0x99, 0x98, 0x6c, 0xf8, 0x22,
        0xd5,
    ];
    assert_eq!(
        parse_md5(b"a0c16616c91907bd14e999986cf822d5"),
        Some(expected)
    );
    assert_eq!(
        parse_md5(b"A0C16616C91907BD14E999986CF822D5"),
        Some(expected)
    );
    assert_eq!(parse_md5(b"a0c16616c91907bd14e999986cf822d"), None);
}

#[test]
fn names_and_errors() {
    assert_eq!(name(opcode::SPI_FLASH_MD5), "SPI_FLASH_MD5");
    assert_eq!(rom_error(0x07), "bad checksum");
    assert_eq!(rom_error(0x01), "unknown error");
}

#[test]
fn detects_chips() {
    use super::chip::flash_size;
    assert_eq!(Chip::from_chip_id(9), Some(Chip::Esp32S3));
    assert_eq!(Chip::from_magic(0x00F0_1D83), Some(Chip::Esp32));
    assert_eq!(Chip::from_magic(0x4361_606F), Some(Chip::Esp32C3));
    assert_eq!(Chip::from_magic(0x1234), None);
    assert_eq!(flash_size(0x17), Some(8 * 1024 * 1024));
    assert_eq!(flash_size(0xFF), None);
    assert_eq!(Chip::Esp32S3.name(), "esp32-s3");
    assert!(!Chip::Esp32.has_encryption_word());
    assert_eq!(Chip::Esp32.spi_registers().data, 0x3FF4_2080);
    assert_eq!(Chip::Esp32C6.spi_registers().miso_length, 0x6000_3028);
    assert_eq!(Chip::Esp32S3.watchdog().unwrap().write_protect, 0x6000_80B0);
    assert!(Chip::Esp32C6.watchdog().is_none());
}
