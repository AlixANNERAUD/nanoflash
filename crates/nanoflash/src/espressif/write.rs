//! Erasing, writing compressed, verifying by MD5 and resetting into the application, through
//! the ROM loader. No `FLASH_DEFL_END` is sent: it would leave the loader before verification.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use md5::{Digest, Md5};
use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush};

use super::command::{self, opcode, words};
use super::{EspressifBackend, reset};
use crate::backend::{Backend, ChipInformation, Progress, ProgressEvent, Region, Segment, Stage};
use crate::error::Error;
use crate::transport::{Duration, PortProvider};

/// The size of the blocks `FLASH_BEGIN` and `FLASH_DEFL_DATA` announce.
const BLOCK_SIZE: u32 = 0x400;
/// The ROM loader erases whole sectors.
const SECTOR_SIZE: u32 = 0x1000;
/// The shortest timeout of the commands whose time grows with their size.
const MINIMUM_TIMEOUT: Duration = Duration::from_secs(10);
/// Seconds per 1 000 000 bytes: erasing, writing a block and computing an MD5 digest.
const ERASE_SECONDS_PER_MEGABYTE: u64 = 30;
const WRITE_SECONDS_PER_MEGABYTE: u64 = 40;
const MD5_SECONDS_PER_MEGABYTE: u64 = 8;
/// The zlib level: the best compression, since the link is slower than the compressor.
const COMPRESSION_LEVEL: u8 = 9;
/// `FLASH_END` with 1: do not reboot, run the application.
const RUN_APPLICATION: u32 = 1;

/// A timeout that grows with `size`, never under ten seconds.
fn scaled_timeout(seconds_per_megabyte: u64, size: u32) -> Duration {
    let milliseconds =
        u64::from(size).saturating_mul(seconds_per_megabyte.saturating_mul(1000)) / 1_000_000;
    Duration::from_millis(milliseconds).max(MINIMUM_TIMEOUT)
}

/// The bytes each `BLOCK_SIZE` block of the zlib stream `compressed` inflates to, as the ROM
/// loader inflates it block by block; `None` when the stream does not inflate.
fn inflated_sizes(compressed: &[u8]) -> Option<Vec<u32>> {
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut output = alloc::vec![0_u8; 0x8000];
    let mut sizes = Vec::new();
    for block in compressed.chunks(BLOCK_SIZE as usize) {
        let mut input = block;
        let mut inflated: u32 = 0;
        // Every pass consumes input or produces output, both finite; a pass doing neither ends
        // the block.
        loop {
            let result = inflate(&mut state, input, &mut output, MZFlush::None);
            if let Err(error) = result.status {
                if error != MZError::Buf {
                    return None;
                }
            }
            input = input.get(result.bytes_consumed..)?;
            inflated = inflated.checked_add(u32::try_from(result.bytes_written).ok()?)?;
            if result.bytes_consumed == 0 && result.bytes_written == 0 {
                break;
            }
        }
        sizes.push(inflated);
    }
    Some(sizes)
}

/// A segment checked against what the protocol can carry.
struct Checked<'a> {
    label: &'a str,
    offset: u32,
    data: &'a [u8],
    /// The length of the data padded to a multiple of four bytes.
    padded_length: u32,
    /// The padded length rounded up to whole sectors.
    erase_size: u32,
}

impl Checked<'_> {
    /// The data padded with `0xFF` to a multiple of four bytes.
    fn padded(&self) -> Vec<u8> {
        let mut padded = self.data.to_vec();
        padded.resize(self.padded_length as usize, 0xFF);
        padded
    }
}

fn out_of_range(label: &str, what: &str, value: u64) -> Error {
    Error::OutOfRange {
        what: format!("{label} {what}"),
        value,
    }
}

fn check<'a>(segment: &Segment<'a>) -> Result<Checked<'a>, Error> {
    let offset = u32::try_from(segment.offset)
        .map_err(|_| out_of_range(segment.label, "offset", segment.offset))?;
    let length = segment.data.len() as u64;
    let too_large = || out_of_range(segment.label, "size", length);
    let padded_length = u32::try_from(length)
        .ok()
        .and_then(|length| length.checked_next_multiple_of(4))
        .ok_or_else(too_large)?;
    let erase_size = padded_length
        .checked_next_multiple_of(SECTOR_SIZE)
        .ok_or_else(too_large)?;
    Ok(Checked {
        label: segment.label,
        offset,
        data: segment.data,
        padded_length,
        erase_size,
    })
}

/// Checks every segment before any is sent, so a bad one leaves the flash untouched.
fn check_all<'a>(segments: &[Segment<'a>]) -> Result<Vec<Checked<'a>>, Error> {
    segments.iter().map(check).collect()
}

fn hexadecimal(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

impl<P: PortProvider> EspressifBackend<'_, P> {
    /// Whether `FLASH_BEGIN` and `FLASH_DEFL_BEGIN` carry the encryption word. Before a
    /// connection the chip is unknown: every chip but the ESP32 carries it.
    fn encryption_word(&self) -> bool {
        self.chip.is_none_or(|chip| chip.has_encryption_word())
    }

    async fn write_segment(
        &mut self,
        segment: &Checked<'_>,
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let padded = segment.padded();
        let compressed = compress_to_vec_zlib(&padded, COMPRESSION_LEVEL);
        let blocks = u32::try_from(compressed.len().div_ceil(BLOCK_SIZE as usize))
            .map_err(|_| out_of_range("compressed image", "size", compressed.len() as u64))?;
        let begin = command::begin_data(
            segment.erase_size,
            blocks,
            BLOCK_SIZE,
            segment.offset,
            self.encryption_word(),
        );
        self.command_with(
            opcode::FLASH_DEFL_BEGIN,
            &begin,
            0,
            scaled_timeout(ERASE_SECONDS_PER_MEGABYTE, segment.erase_size),
        )
        .await?;
        // Each block is written once inflated: its timeout grows with what it inflates to, as in
        // espflash. A stream that does not inflate here keeps the timeout of the whole image.
        let inflated = inflated_sizes(&compressed).unwrap_or_default();
        let total = segment.data.len() as u64;
        for (sequence, chunk) in (0_u32..).zip(compressed.chunks(BLOCK_SIZE as usize)) {
            let size = inflated
                .get(sequence as usize)
                .copied()
                .unwrap_or(segment.padded_length);
            let timeout = scaled_timeout(WRITE_SECONDS_PER_MEGABYTE, size);
            let (payload, checksum) = command::block(chunk, sequence);
            self.command_with(opcode::FLASH_DEFL_DATA, &payload, checksum, timeout)
                .await?;
            let done = total
                .saturating_mul(u64::from(sequence).saturating_add(1))
                .checked_div(u64::from(blocks))
                .unwrap_or(total);
            progress.report(ProgressEvent {
                stage: Stage::Writing,
                offset: u64::from(segment.offset),
                done,
                total,
            });
        }
        Ok(())
    }

    async fn verify_segment(
        &mut self,
        segment: &Checked<'_>,
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let total = segment.data.len() as u64;
        let mut report = |done| {
            progress.report(ProgressEvent {
                stage: Stage::Verifying,
                offset: u64::from(segment.offset),
                done,
                total,
            });
        };
        report(0);
        let response = self
            .command_with(
                opcode::SPI_FLASH_MD5,
                &words(&[segment.offset, segment.padded_length, 0, 0]),
                0,
                scaled_timeout(MD5_SECONDS_PER_MEGABYTE, segment.padded_length),
            )
            .await?;
        let actual = command::parse_md5(&response.data).ok_or_else(|| Error::InvalidResponse {
            command: command::name(opcode::SPI_FLASH_MD5),
            message: format!(
                "expected 32 hexadecimal digits, got {} bytes",
                response.data.len()
            ),
        })?;
        let expected = Md5::digest(segment.padded());
        if expected.as_slice() != actual {
            return Err(Error::VerificationFailed {
                label: segment.label.into(),
                offset: u64::from(segment.offset),
                size: u64::from(segment.padded_length),
                expected: hexadecimal(&expected),
                actual: hexadecimal(&actual),
            });
        }
        report(total);
        Ok(())
    }

    /// Starts the application from the ROM loader: an empty `FLASH_BEGIN`, then `FLASH_END`
    /// asking to run the application, which the loader does not answer.
    async fn soft_reset(&mut self) -> Result<(), Error> {
        let begin = command::begin_data(0, 0, BLOCK_SIZE, 0, self.encryption_word());
        self.command_with(opcode::FLASH_BEGIN, &begin, 0, MINIMUM_TIMEOUT)
            .await?;
        self.send(opcode::FLASH_END, &words(&[RUN_APPLICATION]), 0)
            .await
    }
}

impl<P: PortProvider> Backend for EspressifBackend<'_, P> {
    async fn connect(&mut self) -> Result<ChipInformation, Error> {
        // The orchestration reports `Connecting` itself.
        EspressifBackend::connect(self, &mut ()).await
    }

    async fn erase(&mut self, region: Region) -> Result<(), Error> {
        let offset = u32::try_from(region.offset)
            .map_err(|_| out_of_range("erase", "offset", region.offset))?;
        let size = u32::try_from(region.size)
            .ok()
            .and_then(|size| size.checked_next_multiple_of(SECTOR_SIZE))
            .ok_or_else(|| out_of_range("erase", "size", region.size))?;
        let begin = command::begin_data(size, 0, BLOCK_SIZE, offset, self.encryption_word());
        self.command_with(
            opcode::FLASH_BEGIN,
            &begin,
            0,
            scaled_timeout(ERASE_SECONDS_PER_MEGABYTE, size),
        )
        .await?;
        Ok(())
    }

    async fn write(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        for segment in check_all(segments)? {
            self.write_segment(&segment, progress).await?;
        }
        Ok(())
    }

    async fn verify(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        for segment in check_all(segments)? {
            self.verify_segment(&segment, progress).await?;
        }
        Ok(())
    }

    async fn reset_to_application(&mut self) -> Result<(), Error> {
        if !reset::reset_to_application(&mut self.port, &self.reset).await? {
            self.soft_reset().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_grow_with_the_size() {
        assert_eq!(scaled_timeout(30, 0x1000), MINIMUM_TIMEOUT);
        assert_eq!(
            scaled_timeout(30, 0x80_0000),
            Duration::from_millis(251_658)
        );
        assert_eq!(
            scaled_timeout(40, u32::MAX),
            Duration::from_millis(171_798_691)
        );
    }

    #[test]
    fn blocks_inflate_to_the_whole_image() {
        let image = alloc::vec![0xFF_u8; 4 * 1024 * 1024];
        let compressed = compress_to_vec_zlib(&image, COMPRESSION_LEVEL);
        let sizes = inflated_sizes(&compressed).unwrap();
        assert_eq!(sizes.len(), compressed.len().div_ceil(BLOCK_SIZE as usize));
        assert!(sizes.len() > 1, "{sizes:?}");
        assert_eq!(
            sizes.iter().map(|&size| u64::from(size)).sum::<u64>(),
            4 * 1024 * 1024
        );
        assert!(
            sizes.iter().all(|&size| size < 4 * 1024 * 1024),
            "{sizes:?}"
        );
        assert_eq!(inflated_sizes(&[0x78, 0x9C, 0xFF, 0xFF]), None);
    }

    #[test]
    fn sizes_are_padded_and_rounded() {
        let data = [0_u8; 5];
        let checked = check(&Segment {
            label: "image",
            offset: 0x1000,
            data: &data,
        })
        .unwrap();
        assert_eq!(
            (checked.offset, checked.padded_length, checked.erase_size),
            (0x1000, 8, 0x1000)
        );
        assert_eq!(checked.padded(), [0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF]);
    }
}
