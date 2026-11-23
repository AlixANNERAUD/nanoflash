// Command sequences, timeouts and the pause after the data of `S` come from observing bossac (arduino/BOSSA, BSD-3-Clause, behaviour reference only) and the Arduino bootloaders (protocol reference only); no code is taken from either.
//! Erasing, writing, checking and resetting through Arduino's SAM-BA bootloaders.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::family::{self, Family};
use super::{LINE_END, SambaBackend, Session, command};
use crate::backend::{Backend, ChipInformation, Progress, ProgressEvent, Region, Segment, Stage};
use crate::error::Error;
use crate::transport::{Duration, PortProvider, SerialPort, TransportError};

/// How long `X` may take: the SAMD21 erases up to 256 KiB.
const ERASE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the selection of the RAM buffer may take.
const SELECT_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the copy of the RAM buffer to flash may take, erasing included on the nRF52840.
const COPY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the CRC of a chunk may take.
const CHECKSUM_TIMEOUT: Duration = Duration::from_secs(5);
/// The pause after the data of `S` before the next command, as bossac makes.
const DATA_PAUSE: Duration = Duration::from_millis(10);
/// The longest reply to `X`, `Y` and `Z`.
const REPLY_BUDGET: usize = 64;

fn not_connected() -> Error {
    Error::NoBootloader { waited_for: None }
}

fn size_of(data: &[u8]) -> u64 {
    u64::try_from(data.len()).unwrap_or(u64::MAX)
}

fn out_of_range(name: &str, value: u64) -> Error {
    Error::OutOfRange {
        what: format!("{name} offset"),
        value,
    }
}

fn report(progress: &mut dyn Progress, stage: Stage, offset: u64, done: u64, total: u64) {
    progress.report(ProgressEvent {
        stage,
        offset,
        done,
        total,
    });
}

/// The address a command carries for the flash offset `offset` of `name`.
fn address(family: &Family, name: &str, offset: u64) -> Result<u32, Error> {
    family
        .command_address(offset)
        .ok_or_else(|| out_of_range(name, offset))
}

fn is_multiple(value: u64, unit: u64) -> bool {
    value.checked_rem(unit) == Some(0)
}

/// Checks that `offset..offset + size` of `what` lies in the flash the bootloader writes and
/// that commands can address it; returns its exclusive end.
fn check_range(
    family: &Family,
    what: &dyn Fn() -> String,
    name: &str,
    offset: u64,
    size: u64,
) -> Result<u64, Error> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| out_of_range(name, offset))?;
    if offset < family.application_start {
        return Err(Error::BelowWritableFlash {
            what: what(),
            offset,
            writable_from: family.application_start,
        });
    }
    if end > family.flash_size {
        return Err(Error::OutsideDetectedFlash {
            what: what(),
            end,
            flash_size: family.flash_size,
        });
    }
    address(family, name, offset)?;
    address(family, name, end)?;
    Ok(end)
}

/// Checks that `segment` lies in the writable flash and starts on the write unit, so that its
/// padded chunks stay inside the flash.
fn check_segment(family: &Family, segment: &Segment<'_>) -> Result<(), Error> {
    let what = || format!("image \"{}\"", segment.label);
    check_range(
        family,
        &what,
        segment.label,
        segment.offset,
        size_of(segment.data),
    )?;
    if !is_multiple(segment.offset, family.write_unit) {
        return Err(Error::MisalignedImage {
            label: segment.label.into(),
            offset: segment.offset,
            required: family.write_unit,
        });
    }
    Ok(())
}

/// Checks that `region` lies in the writable flash on whole write units and, where `X` erases
/// to the end of flash, that it ends there.
fn check_region(family: &Family, region: Region) -> Result<(), Error> {
    let what = || format!("erase region at 0x{:x}", region.offset);
    let end = check_range(family, &what, "erase", region.offset, region.size)?;
    if !is_multiple(region.offset, family.write_unit)
        || !is_multiple(region.size, family.write_unit)
    {
        return Err(Error::MisalignedErase {
            entry: format!("region at 0x{:x}", region.offset),
            offset: region.offset,
            size: region.size,
            required: family.write_unit,
        });
    }
    if family.erase_to_end && end != family.flash_size {
        return Err(Error::EraseShortOfFlashEnd {
            chip: family.chip.into(),
            offset: region.offset,
            end,
            flash_size: family.flash_size,
        });
    }
    Ok(())
}

/// Whether a write at `offset` needs an `X` first: nothing below it was erased yet.
fn needs_erase(erased_from: Option<u64>, offset: u64) -> bool {
    erased_from.is_none_or(|from| from > offset)
}

fn erase_message(family: &Family, offset: u64) -> String {
    format!(
        "the SAMD21 bootloader erases from 0x{offset:x} to the end of flash at 0x{:x}: what was stored after the images is gone",
        family.flash_size
    )
}

impl<P: PortProvider> SambaBackend<'_, P> {
    fn session(&self) -> Result<&Session, Error> {
        self.session.as_ref().ok_or_else(not_connected)
    }

    /// Reads the reply of `letter`: a line of at most 64 bytes, longer when it is invalid.
    async fn reply(&mut self, letter: &'static str, timeout: Duration) -> Result<Vec<u8>, Error> {
        let reply = self
            .read_line(timeout, REPLY_BUDGET.saturating_add(1))
            .await?;
        if reply.is_empty() {
            return Err(Error::NoResponse { command: letter });
        }
        Ok(reply)
    }

    /// Reads the acknowledgement of `letter`: the letter, maybe more, and `\n\r`.
    async fn acknowledgement(
        &mut self,
        letter: &'static str,
        timeout: Duration,
    ) -> Result<(), Error> {
        let reply = self.reply(letter, timeout).await?;
        if reply.len() <= REPLY_BUDGET
            && reply.ends_with(LINE_END)
            && reply.starts_with(letter.as_bytes())
        {
            return Ok(());
        }
        Err(Error::InvalidResponse {
            command: letter,
            message: format!("expected {letter}\\n\\r, got {}", command::line(&reply)),
        })
    }

    /// Sends `X` at `offset`: the SAMD21 erases from there to the end of flash, the nRF52840
    /// erases nothing.
    async fn erase_from(&mut self, family: &Family, offset: u64) -> Result<(), Error> {
        let address = address(family, "erase", offset)?;
        self.port()?.write(&command::erase(address)).await?;
        self.acknowledgement("X", ERASE_TIMEOUT).await?;
        if let Some(session) = self.session.as_mut() {
            session.erased_from = Some(session.erased_from.map_or(offset, |from| from.min(offset)));
        }
        Ok(())
    }

    /// Writes `data`, one buffer at most, at the flash offset `offset` of `name`: `S` stores it
    /// padded with `0xFF` to the write unit in the RAM buffer, `Y` selects the buffer and `Y`
    /// copies it to flash.
    async fn write_chunk(
        &mut self,
        family: &Family,
        name: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        let address = address(family, name, offset)?;
        let padded_length = usize::try_from(family.write_unit)
            .ok()
            .and_then(|unit| data.len().checked_next_multiple_of(unit))
            .ok_or_else(|| out_of_range(name, offset))?;
        let length = u32::try_from(padded_length).map_err(|_| out_of_range(name, offset))?;
        let mut block = data.to_vec();
        block.resize(padded_length, 0xFF);
        let port = self.port()?;
        port.write(&command::send(family.buffer, length)).await?;
        port.write(&block).await?;
        port.sleep(DATA_PAUSE).await;
        port.write(&command::select_buffer(family.buffer)).await?;
        self.acknowledgement("Y", SELECT_TIMEOUT).await?;
        self.port()?.write(&command::copy(address, length)).await?;
        self.acknowledgement("Y", COPY_TIMEOUT).await
    }

    /// Erases where needed, then writes `segment` one buffer at a time.
    async fn write_segment(
        &mut self,
        family: &Family,
        segment: &Segment<'_>,
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let total = size_of(segment.data);
        if total == 0 {
            return Ok(());
        }
        if needs_erase(self.session()?.erased_from, segment.offset) {
            if family.erase_to_end {
                let erased = family.flash_size.saturating_sub(segment.offset);
                report(progress, Stage::Erasing, segment.offset, 0, erased);
                progress.message(&erase_message(family, segment.offset));
                self.erase_from(family, segment.offset).await?;
                report(progress, Stage::Erasing, segment.offset, erased, erased);
            } else {
                // Sent as bossac does; the pages are erased by the copies, inside `Writing`.
                self.erase_from(family, segment.offset).await?;
            }
        }
        if let Some(session) = self.session.as_mut() {
            session.written = true;
        }
        let mut offset = segment.offset;
        let mut done: u64 = 0;
        for chunk in segment.data.chunks(family::BUFFER_SIZE) {
            self.write_chunk(family, segment.label, offset, chunk)
                .await?;
            offset = offset.saturating_add(size_of(chunk));
            done = done.saturating_add(size_of(chunk));
            report(progress, Stage::Writing, segment.offset, done, total);
        }
        Ok(())
    }

    /// Checks `segment` one buffer at a time against the CRC-16 the bootloader computes.
    async fn verify_segment(
        &mut self,
        family: &Family,
        segment: &Segment<'_>,
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let total = size_of(segment.data);
        let mut offset = segment.offset;
        let mut done: u64 = 0;
        for chunk in segment.data.chunks(family::BUFFER_SIZE) {
            let address = address(family, segment.label, offset)?;
            let size = size_of(chunk);
            let length =
                u32::try_from(chunk.len()).map_err(|_| out_of_range(segment.label, offset))?;
            self.port()?
                .write(&command::checksum(address, length))
                .await?;
            let reply = self.reply("Z", CHECKSUM_TIMEOUT).await?;
            let complete = reply.len() <= REPLY_BUDGET && reply.ends_with(LINE_END);
            let actual = complete
                .then(|| command::parse_checksum(&reply))
                .flatten()
                .ok_or_else(|| Error::InvalidResponse {
                    command: "Z",
                    message: format!(
                        "expected Z, eight hexadecimal digits of a 16-bit value and #\\n\\r, got {}",
                        command::line(&reply)
                    ),
                })?;
            let expected = command::crc16(chunk);
            if actual != expected {
                return Err(Error::VerificationFailed {
                    label: segment.label.into(),
                    offset,
                    size,
                    expected: format!("{expected:04x}"),
                    actual: format!("{actual:04x}"),
                    checksum: "CRC-16",
                });
            }
            offset = offset.saturating_add(size);
            done = done.saturating_add(size);
            report(progress, Stage::Verifying, segment.offset, done, total);
        }
        Ok(())
    }
}

impl<P: PortProvider> Backend for SambaBackend<'_, P> {
    async fn connect(&mut self) -> Result<ChipInformation, Error> {
        // The orchestration reports `Connecting` itself.
        SambaBackend::connect(self, &mut ()).await
    }

    /// On the SAMD21, one `X`, so `region` must end at the end of flash. On the nRF52840, whose
    /// `X` erases nothing and whose copies erase the pages they program, every 4 KiB page of
    /// `region` is written with `0xFF`, reported between the orchestration's own events.
    async fn erase(&mut self, region: Region, progress: &mut dyn Progress) -> Result<(), Error> {
        let family = self.session()?.family;
        if region.size == 0 {
            return Ok(());
        }
        check_region(family, region)?;
        if family.erase_to_end {
            progress.message(&erase_message(family, region.offset));
            return self.erase_from(family, region.offset).await;
        }
        let page = vec![0xFF; family::BUFFER_SIZE];
        let mut offset = region.offset;
        let mut done: u64 = 0;
        while done < region.size {
            let length = region.size.saturating_sub(done).min(size_of(&page));
            let bytes = usize::try_from(length)
                .ok()
                .and_then(|length| page.get(..length))
                .unwrap_or_default();
            self.write_chunk(family, "erase", offset, bytes).await?;
            offset = offset.saturating_add(length);
            done = done.saturating_add(length);
            if done < region.size {
                report(progress, Stage::Erasing, region.offset, done, region.size);
            }
        }
        Ok(())
    }

    /// Writes `segments` in order, checking them all first. Before the first write at or below
    /// an offset, `X` erases from there: on the SAMD21 to the end of flash, reported as
    /// `Erasing` with a message, so a segment below one already written is refused.
    async fn write(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let session = self.session()?;
        let family = session.family;
        let mut erased_from = session.erased_from;
        let mut written = session.written;
        for segment in segments {
            check_segment(family, segment)?;
            if segment.data.is_empty() {
                continue;
            }
            if needs_erase(erased_from, segment.offset) {
                if family.erase_to_end && written {
                    return Err(Error::ImageBelowWritten {
                        chip: family.chip.into(),
                        label: segment.label.into(),
                        offset: segment.offset,
                    });
                }
                erased_from = Some(segment.offset);
            }
            written = true;
        }
        for segment in segments {
            self.write_segment(family, segment, progress).await?;
        }
        Ok(())
    }

    /// Compares the CRC-16 of every 4 KiB chunk of `segments` with the one the bootloader
    /// computes over the flash.
    async fn verify(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let family = self.session()?.family;
        for segment in segments {
            check_segment(family, segment)?;
        }
        for segment in segments {
            self.verify_segment(family, segment, progress).await?;
        }
        Ok(())
    }

    /// Sends `K` when the bootloader has it, else requests a system reset through `AIRCR`; no
    /// reply is read, and a board that leaves during the write has reset. `G` is not used: it
    /// jumps into the application without resetting the clocks and USB the bootloader set up.
    async fn reset_to_application(&mut self) -> Result<(), Error> {
        let request = if self.session()?.capabilities.reset {
            command::RESET.to_vec()
        } else {
            command::write_word(family::AIRCR, family::SYSTEM_RESET)
        };
        match self.port()?.write(&request).await {
            Ok(()) | Err(TransportError::Disconnected) => {}
            Err(error) => return Err(error.into()),
        }
        // The bootloader is gone.
        self.session = None;
        Ok(())
    }
}
