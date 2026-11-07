//! A namespace as a disk: reads and writes with PRP entries, flushes,
//! and discards through Dataset Management.

use crate::controller::Controller;
use crate::queue::Command;
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockFlags, BlockInfo};
use mikros_storage::PAGE;

/// I/O opcodes.
const FLUSH: u8 = 0x00;
const WRITE: u8 = 0x01;
const READ: u8 = 0x02;
const DATASET_MANAGEMENT: u8 = 0x09;
/// Write's force unit access bit, and Dataset Management's deallocate
/// attribute.
const FUA: u32 = 1 << 30;
const DEALLOCATE: u32 = 1 << 2;
/// The ranges one Dataset Management command takes, and the most blocks
/// in one range.
const RANGES_MAX: u64 = 256;
const RANGE_BLOCKS_MAX: u64 = u32::MAX as u64;
/// The most blocks one Read or Write takes.
const COMMAND_BLOCKS_MAX: u64 = 1 << 16;

pub struct Namespace {
    pub nsid: u32,
    pub block_size: u32,
    pub blocks: u64,
    pub controller: Rc<Controller>,
}

impl Namespace {
    /// The most blocks one request moves.
    pub fn max_transfer_blocks(&self) -> u32 {
        let blocks = self.controller.transfer_max() / u64::from(self.block_size);
        blocks.clamp(1, COMMAND_BLOCKS_MAX) as u32
    }

    /// Whether the controller has a volatile write cache, for which
    /// Flush is used.
    pub fn flushes(&self) -> bool {
        self.controller.identity.write_cache
    }

    /// Whether the controller supports Dataset Management.
    pub fn discards(&self) -> bool {
        self.controller.identity.dataset_management
    }

    /// Reads, or writes when `write` is true, the `len` bytes at byte
    /// `offset` of the memory whose pages are at the physical addresses
    /// `pages`, from or to the blocks from `block`. `fua` sets Force Unit
    /// Access on a write. `offset` must be a multiple of 4, and `len` a
    /// multiple of the block size of at most `max_transfer_blocks`
    /// blocks, within `pages`.
    pub async fn transfer(
        &self,
        write: bool,
        block: u64,
        pages: &[u64],
        offset: u64,
        len: u64,
        fua: bool,
    ) -> Result<(), Status> {
        if self.controller.failed.get() {
            return Err(Status::Internal);
        }
        let first = (offset / PAGE) as usize;
        let within = offset % PAGE;
        let count = (within + len).div_ceil(PAGE) as usize;
        let touched = &pages[first..first + count];
        let prp1 = touched[0] + within;
        let mut list = None;
        let prp2 = match count {
            1 => 0,
            2 => touched[1],
            _ => {
                let page = self.controller.page()?;
                for (i, address) in touched[1..].iter().enumerate() {
                    page.write64(8 * i as u64, *address);
                }
                let at = page.physical(0);
                list = Some(page);
                at
            }
        };
        let blocks = (len / u64::from(self.block_size)) as u32;
        let mut cdw12 = blocks - 1;
        if write && fua {
            cdw12 |= FUA;
        }
        let command = Command {
            opcode: if write { WRITE } else { READ },
            nsid: self.nsid,
            prp1,
            prp2,
            cdw: [block as u32, (block >> 32) as u32, cdw12, 0, 0, 0],
        };
        let done = self.controller.io.submit(command).await;
        if let Some(page) = list {
            self.controller.give_page(page);
        }
        done.map(drop).map_err(Status::from)
    }

    /// Flushes the volatile write cache.
    pub async fn flush(&self) -> Result<(), Status> {
        if self.controller.failed.get() {
            return Err(Status::Internal);
        }
        let command = Command {
            opcode: FLUSH,
            nsid: self.nsid,
            ..Command::default()
        };
        self.controller
            .io
            .submit(command)
            .await
            .map(drop)
            .map_err(Status::from)
    }

    /// Deallocates the `count` blocks from `block`, with as many Dataset
    /// Management commands as their ranges need.
    pub async fn discard(&self, mut block: u64, mut count: u64) -> Result<(), Status> {
        while count > 0 {
            if self.controller.failed.get() {
                return Err(Status::Internal);
            }
            let page = self.controller.page()?;
            let mut ranges = 0;
            while count > 0 && ranges < RANGES_MAX {
                let n = count.min(RANGE_BLOCKS_MAX);
                let at = 16 * ranges;
                page.write32(at, 0);
                page.write32(at + 4, n as u32);
                page.write64(at + 8, block);
                block += n;
                count -= n;
                ranges += 1;
            }
            let command = Command {
                opcode: DATASET_MANAGEMENT,
                nsid: self.nsid,
                prp1: page.physical(0),
                cdw: [ranges as u32 - 1, DEALLOCATE, 0, 0, 0, 0],
                ..Command::default()
            };
            let done = self.controller.io.submit(command).await;
            self.controller.give_page(page);
            done.map_err(Status::from)?;
        }
        Ok(())
    }
}

impl mikros_storage::Disk for Namespace {
    fn info(&self) -> BlockInfo {
        let mut flags = BlockFlags::FUA.0;
        if self.flushes() {
            flags |= BlockFlags::FLUSH.0;
        }
        if self.discards() {
            flags |= BlockFlags::DISCARD.0;
        }
        let identity = &self.controller.identity;
        BlockInfo {
            block_size: self.block_size,
            block_count: self.blocks,
            max_transfer_blocks: self.max_transfer_blocks(),
            flags: BlockFlags(flags),
            model: identity.model.clone(),
            serial: identity.serial.clone(),
        }
    }

    fn dma(&self) -> &OwnedHandle {
        &self.controller.dma
    }

    fn bti(&self) -> &OwnedHandle {
        &self.controller.bti
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
        Namespace::transfer(self, write, block, pages, offset, len, fua).await
    }

    async fn flush(&self) -> Result<(), Status> {
        // Without a volatile write cache every completed write is
        // already on stable storage.
        match self.flushes() {
            true => Namespace::flush(self).await,
            false => Ok(()),
        }
    }

    async fn discard(&self, block: u64, count: u64) -> Result<(), Status> {
        Namespace::discard(self, block, count).await
    }
}
