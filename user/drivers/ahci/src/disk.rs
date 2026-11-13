//! A SATA disk on a port: identifying it, and its reads, writes,
//! flushes and discards.

use crate::ata::{
    Ata, DATA_SET_MANAGEMENT, DEVICE_LBA, FLUSH_CACHE_EXT, IDENTIFY_DEVICE, Identity, read_write,
};
use crate::port::{PRD_MAX, Port};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockFlags, BlockInfo};
use mikros_rt::sys;
use mikros_storage::{Contiguous, PAGE};

/// DATA SET MANAGEMENT's TRIM bit, the size of one LBA range entry and
/// the most sectors in one.
const TRIM: u16 = 1 << 0;
const RANGE_SIZE: u64 = 8;
const RANGE_SECTORS_MAX: u64 = 0xFFFF;
/// The most sectors one read or write moves.
const COMMAND_SECTORS_MAX: u64 = 1 << 16;
/// The most bytes a command table's PRD entries cover at any offset that
/// is a multiple of 4: such a transfer touches at most one page more
/// than its length fills.
const TRANSFER_MAX: u64 = (PRD_MAX as u64 - 1) * PAGE;
/// The size of IDENTIFY DEVICE's data.
const IDENTIFY_SIZE: u32 = 512;

/// Sends IDENTIFY DEVICE to the disk on `port`, into `page`.
pub async fn identify(port: &Port, page: &Contiguous) -> Result<Identity, Status> {
    let ata = Ata {
        command: IDENTIFY_DEVICE,
        ..Ata::default()
    };
    port.issue(ata, &[(page.physical(0), IDENTIFY_SIZE)], false, false)
        .await?;
    let mut w = [0u16; 256];
    for (i, word) in w.iter_mut().enumerate() {
        *word = page.read16(2 * i as u64);
    }
    let Some(identity) = Identity::parse(&w) else {
        mikros_rt::log!(
            Info,
            "ahci: skipping the disk on port {}: its logical sectors are not a power of two of at least 512 bytes",
            port.number
        );
        return Err(Status::NotSupported);
    };
    Ok(identity)
}

pub struct Disk {
    pub port: Rc<Port>,
    pub identity: Identity,
    /// Whether reads and writes use native command queuing.
    pub queued: bool,
    /// Where pages for the LBA ranges of discards come from, and those
    /// that are free.
    pub memory: Rc<Memory>,
    pub spare: RefCell<Vec<Contiguous>>,
    /// The disk's writable connections.
    pub writer: mikros_storage::Writer,
}

/// Where the driver's DMA memory comes from: the `DMA` resource, the
/// bus transaction initiator, the process it is mapped into, and
/// whether it must lie below 4 GiB.
pub struct Memory {
    pub dma: OwnedHandle,
    pub bti: OwnedHandle,
    pub process: sys::Handle,
    pub below_4g: bool,
}

impl Memory {
    pub fn contiguous(&self, size: u64) -> Result<Contiguous, Status> {
        Contiguous::new(&self.dma, &self.bti, self.process, size, self.below_4g)
    }
}

impl Disk {
    /// The most blocks one request moves: the smaller of 65536 sectors
    /// and what one command table describes.
    pub fn max_transfer_blocks(&self) -> u32 {
        let size = u64::from(self.identity.sector_size);
        (TRANSFER_MAX / size).clamp(1, COMMAND_SECTORS_MAX) as u32
    }

    /// Reads, or writes when `write` is true, the `len` bytes at byte
    /// `offset` of the memory whose pages are at the physical addresses
    /// `pages`, from or to the blocks from `block`. `offset` must be even,
    /// and `len` a multiple of the sector size of at most
    /// `max_transfer_blocks` sectors, within `pages`. A write with `fua`
    /// forces unit access when the disk supports it, with WRITE FPDMA
    /// QUEUED's FUA bit or WRITE DMA FUA EXT, and is followed by a flush
    /// otherwise.
    pub async fn transfer(
        &self,
        write: bool,
        block: u64,
        pages: &[u64],
        offset: u64,
        len: u64,
        fua: bool,
    ) -> Result<(), Status> {
        let mut data = Vec::new();
        let (mut at, end) = (offset, offset + len);
        while at < end {
            let piece = (PAGE - at % PAGE).min(end - at);
            let address = pages[(at / PAGE) as usize] + at % PAGE;
            match data.last_mut() {
                Some((last, n)) if *last + u64::from(*n) == address => *n += piece as u32,
                _ => data.push((address, piece as u32)),
            }
            at += piece;
        }
        if data.len() > PRD_MAX {
            return Err(Status::InvalidArgs);
        }
        let sectors = len / u64::from(self.identity.sector_size);
        // A count of 0 stands for 65536.
        let count = sectors as u16;
        let (ata, flush) = read_write(self.queued, write, fua, self.identity.fua, block, count);
        self.port.issue(ata, &data, write, self.queued).await?;
        if flush {
            self.flush().await?;
        }
        Ok(())
    }

    /// Flushes the disk's write cache with FLUSH CACHE EXT.
    pub async fn flush(&self) -> Result<(), Status> {
        let ata = Ata {
            command: FLUSH_CACHE_EXT,
            device: DEVICE_LBA,
            ..Ata::default()
        };
        self.port.issue(ata, &[], false, false).await
    }

    /// Trims the `count` blocks from `block`, with as many DATA SET
    /// MANAGEMENT commands as their LBA ranges need.
    pub async fn discard(&self, mut block: u64, mut count: u64) -> Result<(), Status> {
        let page = match self.spare.borrow_mut().pop() {
            Some(page) => page,
            None => self.memory.contiguous(PAGE)?,
        };
        let done = self.trim(&page, &mut block, &mut count).await;
        self.spare.borrow_mut().push(page);
        done
    }

    async fn trim(
        &self,
        page: &Contiguous,
        block: &mut u64,
        count: &mut u64,
    ) -> Result<(), Status> {
        while *count > 0 {
            page.zero();
            let mut ranges = 0;
            while *count > 0 && ranges < PAGE / RANGE_SIZE {
                let n = (*count).min(RANGE_SECTORS_MAX);
                page.write64(RANGE_SIZE * ranges, *block | n << 48);
                *block += n;
                *count -= n;
                ranges += 1;
            }
            let ata = Ata {
                command: DATA_SET_MANAGEMENT,
                features: TRIM,
                // In 512-byte blocks of range entries.
                count: (PAGE / 512) as u16,
                device: DEVICE_LBA,
                ..Ata::default()
            };
            self.port
                .issue(ata, &[(page.physical(0), PAGE as u32)], true, false)
                .await?;
        }
        Ok(())
    }
}

impl mikros_storage::Disk for Disk {
    fn info(&self) -> BlockInfo {
        let mut flags = 0;
        if self.identity.write_cache {
            flags |= BlockFlags::FLUSH.0;
        }
        if self.identity.trim {
            flags |= BlockFlags::DISCARD.0;
        }
        if self.identity.fua {
            flags |= BlockFlags::FUA.0;
        }
        BlockInfo {
            block_size: self.identity.sector_size,
            block_count: self.identity.sectors,
            max_transfer_blocks: self.max_transfer_blocks(),
            flags: BlockFlags(flags),
            model: self.identity.model.clone(),
            serial: self.identity.serial.clone(),
        }
    }

    fn dma(&self) -> &OwnedHandle {
        &self.memory.dma
    }

    fn bti(&self) -> Option<&OwnedHandle> {
        Some(&self.memory.bti)
    }

    fn below_4g(&self) -> bool {
        self.memory.below_4g
    }

    async fn transfer(
        &self,
        write: bool,
        block: u64,
        pages: &[u64],
        offset: u64,
        len: u64,
        fua: bool,
    ) -> Result<(), Status> {
        Disk::transfer(self, write, block, pages, offset, len, fua).await
    }

    async fn flush(&self) -> Result<(), Status> {
        Disk::flush(self).await
    }

    async fn discard(&self, block: u64, count: u64) -> Result<(), Status> {
        Disk::discard(self, block, count).await
    }
}
