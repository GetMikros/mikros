//! A SATA disk on a port: identifying it, and its reads, writes,
//! flushes and discards.

use crate::port::{Ata, DEVICE_LBA, PRD_MAX, Port};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockFlags, BlockInfo};
use mikros_rt::sys;
use mikros_storage::{Contiguous, PAGE};

/// ATA commands.
const READ_DMA_EXT: u8 = 0x25;
const WRITE_DMA_EXT: u8 = 0x35;
const READ_FPDMA_QUEUED: u8 = 0x60;
const WRITE_FPDMA_QUEUED: u8 = 0x61;
const FLUSH_CACHE_EXT: u8 = 0xEA;
const IDENTIFY_DEVICE: u8 = 0xEC;
/// The most sectors one read or write moves, and the most bytes, which
/// a command table's PRD entries always cover.
const COMMAND_SECTORS_MAX: u64 = 1 << 16;
const TRANSFER_MAX: u64 = 1 << 20;
/// The size of IDENTIFY DEVICE's data.
const IDENTIFY_SIZE: u32 = 512;

/// What IDENTIFY DEVICE says about a disk.
pub struct Identity {
    pub model: String,
    pub serial: String,
    pub lba48: bool,
    pub sectors: u64,
    pub sector_size: u32,
    /// Native command queuing, and its queue depth.
    pub ncq: bool,
    pub queue_depth: u32,
    pub write_cache: bool,
}

/// An identify string: ASCII with the bytes of each word swapped, padded
/// with spaces.
fn text(words: &[u16]) -> String {
    let text: String = words
        .iter()
        .flat_map(|w| [(w >> 8) as u8, *w as u8])
        .map(|b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                ' '
            }
        })
        .collect();
    String::from(text.trim())
}

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
    let sector_size = match w[106] & 0xC000 == 0x4000 && w[106] & 1 << 12 != 0 {
        true => 2 * (u32::from(w[117]) | u32::from(w[118]) << 16),
        false => 512,
    };
    let sectors = (0..4).fold(0u64, |n, i| n | u64::from(w[100 + i]) << (16 * i));
    Ok(Identity {
        model: text(&w[27..47]),
        serial: text(&w[10..20]),
        lba48: w[83] & 1 << 10 != 0,
        sectors,
        sector_size,
        ncq: w[76] & 1 << 8 != 0,
        queue_depth: u32::from(w[75] & 0x1F) + 1,
        write_cache: w[85] & 1 << 5 != 0,
    })
}

pub struct Disk {
    pub port: Rc<Port>,
    pub identity: Identity,
    /// Whether reads and writes use native command queuing.
    pub queued: bool,
    /// Where the driver's DMA memory comes from.
    pub memory: Rc<Memory>,
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
    /// The most blocks one request moves.
    pub fn max_transfer_blocks(&self) -> u32 {
        let size = u64::from(self.identity.sector_size);
        (TRANSFER_MAX / size).clamp(1, COMMAND_SECTORS_MAX) as u32
    }

    /// Reads, or writes when `write` is true, the `len` bytes at byte
    /// `offset` of the memory whose pages are at the physical addresses
    /// `pages`, from or to the blocks from `block`. `offset` must be even,
    /// and `len` a multiple of the sector size of at most
    /// `max_transfer_blocks` sectors, within `pages`. A write with `fua`
    /// is followed by a flush.
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
        let ata = match (self.queued, write) {
            (true, _) => Ata {
                command: if write {
                    WRITE_FPDMA_QUEUED
                } else {
                    READ_FPDMA_QUEUED
                },
                features: count,
                lba: block,
                device: DEVICE_LBA,
                ..Ata::default()
            },
            (false, _) => Ata {
                command: if write { WRITE_DMA_EXT } else { READ_DMA_EXT },
                lba: block,
                count,
                device: DEVICE_LBA,
                ..Ata::default()
            },
        };
        self.port.issue(ata, &data, write, self.queued).await?;
        if write && fua {
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
}

impl mikros_storage::Disk for Disk {
    fn info(&self) -> BlockInfo {
        let mut flags = 0;
        if self.identity.write_cache {
            flags |= BlockFlags::FLUSH.0;
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

    fn bti(&self) -> &OwnedHandle {
        &self.memory.bti
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

    async fn discard(&self, _block: u64, _count: u64) -> Result<(), Status> {
        Err(Status::NotSupported)
    }
}
