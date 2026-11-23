// Command bytes and answers from Atmel's application notes AVR061, AVR068 and AVR109, and from observing avrdude (GPL-2.0-or-later) and the Arduino bootloaders (Optiboot, ATmegaBOOT, the wiring bootloader, Caterina); behaviour references only, no code is taken from any of them.

//! Writing, reading back, erasing and resetting through the AVR bootloaders, page by page.
//!
//! The bootloaders have no checksum command: what was written is read back and compared. They
//! have no usable chip erase either (Optiboot ignores it, and Arduino never sends it): a page is
//! erased by programming it with `0xFF`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{
    AvrBackend, COMMAND_TIMEOUT, Dialect, avr109, excerpt, hexadecimal, stk500v1, stk500v2,
};
use crate::backend::{Backend, ChipInformation, Progress, ProgressEvent, Region, Segment, Stage};
use crate::error::Error;
use crate::flash::prepare::address_limit;
use crate::manifest::Protocol;
use crate::transport::{Duration, PortProvider, SerialPort, TransportError};

/// How long programming a page may take: the bootloader erases the page first.
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(5);

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

fn is_multiple(value: u64, unit: u64) -> bool {
    value.checked_rem(unit) == Some(0)
}

/// Where pages may go, from what `connect` found.
struct Bounds {
    protocol: Protocol,
    page_size: u64,
    /// The end of the flash the bootloader leaves to the application.
    writable_to: u64,
    /// The end of what the protocol can address.
    address_limit: u64,
}

impl Bounds {
    /// The page size as a length; an error when it is zero or does not fit.
    fn page_length(&self) -> Result<usize, Error> {
        usize::try_from(self.page_size)
            .ok()
            .filter(|length| *length > 0)
            .ok_or_else(|| Error::OutOfRange {
                what: "flash page size".into(),
                value: self.page_size,
            })
    }

    /// Checks that `offset..offset + size` of `what`, padded to whole pages, ends at or below
    /// `writable_to` and within what the protocol addresses.
    fn check_range(
        &self,
        what: &dyn Fn() -> String,
        name: &str,
        offset: u64,
        size: u64,
    ) -> Result<(), Error> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| out_of_range(name, offset))?;
        // The last page is programmed whole. `writable_to` is a whole number of pages for every
        // chip, so this is `end` but for a bootloader size that is not.
        let padded = end
            .checked_next_multiple_of(self.page_size)
            .ok_or_else(|| out_of_range(name, offset))?;
        if padded > self.writable_to {
            return Err(Error::AboveWritableFlash {
                what: what(),
                end,
                writable_to: self.writable_to,
            });
        }
        if padded > self.address_limit {
            return Err(Error::OutsideAddressRange {
                what: what(),
                end,
                limit: self.address_limit,
                protocol: self.protocol,
            });
        }
        Ok(())
    }

    /// Checks that `segment` starts on a page and that its pages may be written.
    fn check_segment(&self, segment: &Segment<'_>) -> Result<(), Error> {
        if !is_multiple(segment.offset, self.page_size) {
            return Err(Error::MisalignedImage {
                label: segment.label.into(),
                offset: segment.offset,
                required: self.page_size,
            });
        }
        let what = || format!("image \"{}\"", segment.label);
        self.check_range(&what, segment.label, segment.offset, size_of(segment.data))
    }

    /// Checks that `region` covers whole pages that may be written.
    fn check_region(&self, region: Region) -> Result<(), Error> {
        let entry = || format!("region at 0x{:x}", region.offset);
        if !is_multiple(region.offset, self.page_size) || !is_multiple(region.size, self.page_size)
        {
            return Err(Error::MisalignedErase {
                entry: entry(),
                offset: region.offset,
                size: region.size,
                required: self.page_size,
            });
        }
        let what = || format!("erase {}", entry());
        self.check_range(&what, "erase", region.offset, region.size)
    }
}

impl<P: PortProvider> AvrBackend<'_, P> {
    /// What `connect` found; `NotConnected` before it succeeded.
    fn bounds(&self) -> Result<Bounds, Error> {
        let protocol = self.dialect.protocol();
        let session = self
            .session
            .as_ref()
            .ok_or(Error::NotConnected { protocol })?;
        Ok(Bounds {
            protocol,
            page_size: session.chip.page_size,
            writable_to: session.writable_to(protocol),
            address_limit: address_limit(protocol),
        })
    }

    /// Sends the AVR109 `request` and checks that the answer is `\r`.
    async fn avr109_command(
        &mut self,
        command: &'static str,
        request: &[u8],
        timeout: Duration,
    ) -> Result<(), Error> {
        self.port()?.write(request).await?;
        let reply = self.read_answer(1, timeout).await?;
        match reply.as_slice() {
            [] => Err(Error::NoResponse { command }),
            [avr109::DONE] => Ok(()),
            [avr109::UNKNOWN] => Err(Error::InvalidResponse {
                command,
                message: "the bootloader does not know the command (3f, ?)".into(),
            }),
            _ => Err(Error::InvalidResponse {
                command,
                message: format!("expected 0d, got {}", hexadecimal(&reply)),
            }),
        }
    }

    /// Sends `body` in the next STK500v2 message and checks that the answer carries no data
    /// after its status.
    async fn stk500v2_command_without_data(
        &mut self,
        command: &'static str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<(), Error> {
        let data = self
            .stk500v2_command(command, body, timeout, stk500v2::answer)
            .await?;
        if data.is_empty() {
            return Ok(());
        }
        Err(Error::InvalidResponse {
            command,
            message: format!("expected nothing after the status, got {}", excerpt(&data)),
        })
    }

    /// Sets the address of the next page command to the flash offset `offset`.
    async fn load_address(&mut self, offset: u64) -> Result<(), Error> {
        let unaddressable = || out_of_range("page", offset);
        match self.dialect {
            Dialect::Stk500v1 => {
                let request = stk500v1::load_address(offset).ok_or_else(unaddressable)?;
                self.stk500v1_command("STK_LOAD_ADDRESS", &request, 0, COMMAND_TIMEOUT)
                    .await?;
            }
            Dialect::Stk500v2 => {
                let body = stk500v2::load_address(offset).ok_or_else(unaddressable)?;
                self.stk500v2_command_without_data("CMD_LOAD_ADDRESS", &body, COMMAND_TIMEOUT)
                    .await?;
            }
            Dialect::Avr109 => {
                let request = avr109::set_address(offset).ok_or_else(unaddressable)?;
                self.avr109_command("AVR109 A", &request, COMMAND_TIMEOUT)
                    .await?;
            }
        }
        Ok(())
    }

    /// Programs the page at `offset` with `data`, a whole page.
    async fn program_page(&mut self, offset: u64, data: &[u8]) -> Result<(), Error> {
        // The commands carry the length in 16 bits.
        u16::try_from(data.len()).map_err(|_| out_of_range("page", offset))?;
        self.load_address(offset).await?;
        match self.dialect {
            Dialect::Stk500v1 => {
                self.stk500v1_command(
                    "STK_PROG_PAGE",
                    &stk500v1::program_page(data),
                    0,
                    PROGRAM_TIMEOUT,
                )
                .await?;
            }
            Dialect::Stk500v2 => {
                self.stk500v2_command_without_data(
                    "CMD_PROGRAM_FLASH_ISP",
                    &stk500v2::program_flash(data),
                    PROGRAM_TIMEOUT,
                )
                .await?;
            }
            Dialect::Avr109 => {
                self.avr109_command("AVR109 B", &avr109::write_block(data), PROGRAM_TIMEOUT)
                    .await?;
            }
        }
        Ok(())
    }

    /// Reads `length` bytes of flash at `offset`, a whole page.
    async fn read_page(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, Error> {
        let count = u16::try_from(length).map_err(|_| out_of_range("page", offset))?;
        self.load_address(offset).await?;
        let (command, data) = match self.dialect {
            Dialect::Stk500v1 => {
                const COMMAND: &str = "STK_READ_PAGE";
                let request = stk500v1::read_page(count);
                let data = self
                    .stk500v1_command(COMMAND, &request, length, COMMAND_TIMEOUT)
                    .await?;
                (COMMAND, data)
            }
            Dialect::Stk500v2 => {
                const COMMAND: &str = "CMD_READ_FLASH_ISP";
                let data = self
                    .stk500v2_command(
                        COMMAND,
                        &stk500v2::read_flash(count),
                        COMMAND_TIMEOUT,
                        stk500v2::answer_with_status,
                    )
                    .await?;
                (COMMAND, data)
            }
            Dialect::Avr109 => {
                const COMMAND: &str = "AVR109 g";
                self.port()?.write(&avr109::read_block(count)).await?;
                let data = self.read_answer(length, COMMAND_TIMEOUT).await?;
                if data.is_empty() {
                    return Err(Error::NoResponse { command: COMMAND });
                }
                (COMMAND, data)
            }
        };
        if data.len() != length {
            return Err(Error::InvalidResponse {
                command,
                message: format!(
                    "expected {length} bytes of flash, got {}: {}",
                    data.len(),
                    excerpt(&data)
                ),
            });
        }
        Ok(data)
    }

    /// Sends the command that leaves the bootloader and reads its answer.
    async fn leave(&mut self) -> Result<(), Error> {
        match self.dialect {
            Dialect::Stk500v1 => self
                .stk500v1_command("STK_LEAVE_PROGMODE", &stk500v1::LEAVE, 0, COMMAND_TIMEOUT)
                .await
                .map(drop),
            Dialect::Stk500v2 => {
                self.stk500v2_command_without_data(
                    "CMD_LEAVE_PROGMODE_ISP",
                    &stk500v2::LEAVE,
                    COMMAND_TIMEOUT,
                )
                .await
            }
            Dialect::Avr109 => {
                self.avr109_command("AVR109 E", &[avr109::EXIT], COMMAND_TIMEOUT)
                    .await
            }
        }
    }
}

impl<P: PortProvider> Backend for AvrBackend<'_, P> {
    async fn connect(&mut self) -> Result<ChipInformation, Error> {
        // The orchestration reports `Connecting` itself.
        AvrBackend::connect(self, &mut ()).await
    }

    /// Programs every page of `region` with `0xFF`, reported between the orchestration's own
    /// events.
    async fn erase(&mut self, region: Region, progress: &mut dyn Progress) -> Result<(), Error> {
        let bounds = self.bounds()?;
        if region.size == 0 {
            return Ok(());
        }
        bounds.check_region(region)?;
        let page = vec![0xFF; bounds.page_length()?];
        let mut offset = region.offset;
        let mut done: u64 = 0;
        while done < region.size {
            self.program_page(offset, &page).await?;
            offset = offset.saturating_add(bounds.page_size);
            done = done.saturating_add(bounds.page_size);
            if done < region.size {
                report(progress, Stage::Erasing, region.offset, done, region.size);
            }
        }
        Ok(())
    }

    /// Writes `segments` in order, page by page, the last page of each padded with `0xFF`;
    /// checks them all first.
    async fn write(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let bounds = self.bounds()?;
        for segment in segments {
            bounds.check_segment(segment)?;
        }
        let page_length = bounds.page_length()?;
        for segment in segments {
            let total = size_of(segment.data);
            let mut offset = segment.offset;
            let mut done: u64 = 0;
            for chunk in segment.data.chunks(page_length) {
                let mut page = vec![0xFF; page_length];
                if let Some(start) = page.get_mut(..chunk.len()) {
                    start.copy_from_slice(chunk);
                }
                self.program_page(offset, &page).await?;
                offset = offset.saturating_add(bounds.page_size);
                done = done.saturating_add(size_of(chunk));
                report(progress, Stage::Writing, segment.offset, done, total);
            }
        }
        Ok(())
    }

    /// Reads every page of `segments` back and compares it with the image; the padding of a
    /// last page is not compared.
    async fn verify(
        &mut self,
        segments: &[Segment<'_>],
        progress: &mut dyn Progress,
    ) -> Result<(), Error> {
        let bounds = self.bounds()?;
        for segment in segments {
            bounds.check_segment(segment)?;
        }
        let page_length = bounds.page_length()?;
        for segment in segments {
            let total = size_of(segment.data);
            let mut offset = segment.offset;
            let mut done: u64 = 0;
            for chunk in segment.data.chunks(page_length) {
                let flash = self.read_page(offset, page_length).await?;
                let difference = chunk
                    .iter()
                    .zip(&flash)
                    .zip(0_u64..)
                    .find(|((expected, actual), _)| expected != actual);
                if let Some(((expected, actual), index)) = difference {
                    return Err(Error::ReadBackDiffers {
                        label: segment.label.into(),
                        offset: offset.saturating_add(index),
                        expected: *expected,
                        actual: *actual,
                    });
                }
                offset = offset.saturating_add(bounds.page_size);
                done = done.saturating_add(size_of(chunk));
                report(progress, Stage::Verifying, segment.offset, done, total);
            }
        }
        Ok(())
    }

    /// Sends `STK_LEAVE_PROGMODE`, `CMD_LEAVE_PROGMODE_ISP` or `E` and reads the answer; a port
    /// that disconnects meanwhile has left the bootloader too.
    async fn reset_to_application(&mut self) -> Result<(), Error> {
        self.bounds()?;
        match self.leave().await {
            Ok(())
            | Err(Error::Transport {
                error: TransportError::Disconnected,
                ..
            }) => {}
            Err(error) => return Err(error),
        }
        // The bootloader is gone.
        self.session = None;
        Ok(())
    }
}
