//! The partition under a volume: its `Block` channel, through one buffer
//! mapped into the server, with sync calls.

use mikros_fat::{Device, Error};
use mikros_rt::interfaces::mikros::storage::{BlockClient, BlockFlags, BlockInfo};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The size of the buffer for requests, the most one request moves.
const BUFFER: u64 = 256 << 10;
const FOREVER: i64 = i64::MAX;

pub struct BlockDevice {
    block: BlockClient,
    buffer: u32,
    at: u64,
    block_size: u64,
    /// The most bytes one request moves.
    max: u64,
    pub info: BlockInfo,
}

impl BlockDevice {
    /// The device behind `block`, with a buffer mapped into `process`.
    /// None when the disk cannot give one.
    pub fn new(block: BlockClient, process: sys::Handle) -> Option<BlockDevice> {
        let info = block.get_info(FOREVER).ok()?.info;
        let block_size = u64::from(info.block_size);
        if block_size == 0 || !BUFFER.is_multiple_of(block_size) {
            return None;
        }
        let created = block.create_buffer(BUFFER, FOREVER).ok()?.ok()?;
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let mapped = unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                created.memory.raw(),
                0,
                BUFFER,
                &mut at,
            )
        };
        if mapped != mikros_rt::OK {
            return None;
        }
        let max = (u64::from(info.max_transfer_blocks.max(1)) * block_size).min(BUFFER);
        Some(BlockDevice {
            block,
            buffer: created.buffer,
            at,
            block_size,
            max,
            info,
        })
    }

    /// The size of the partition in bytes.
    pub fn size(&self) -> u64 {
        self.info.block_count * self.block_size
    }

    pub fn read_only(&self) -> bool {
        self.info.flags.0 & BlockFlags::READ_ONLY.0 != 0
    }

    fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: the buffer is mapped at `at` for `BUFFER` bytes, and only
        // this device uses it.
        unsafe { core::slice::from_raw_parts_mut(self.at as *mut u8, BUFFER as usize) }
    }

    /// Reads `len` bytes, whole blocks, at `at` into the buffer.
    fn read_blocks(&mut self, at: u64, len: u64) -> Result<(), Error> {
        let (block, count) = (at / self.block_size, (len / self.block_size) as u32);
        match self.block.read(self.buffer, 0, block, count, FOREVER) {
            Ok(Ok(_)) => Ok(()),
            _ => Err(Error::Io),
        }
    }

    fn write_blocks(&mut self, at: u64, len: u64) -> Result<(), Error> {
        let (block, count) = (at / self.block_size, (len / self.block_size) as u32);
        match self
            .block
            .write(self.buffer, 0, block, count, false, FOREVER)
        {
            Ok(Ok(_)) => Ok(()),
            _ => Err(Error::Io),
        }
    }

    /// The whole blocks around `len` bytes at `at`, in at most `max`
    /// bytes: where they start and how many bytes they take.
    fn span(&self, at: u64, len: u64) -> (u64, u64) {
        let start = at - at % self.block_size;
        let end = (at + len).next_multiple_of(self.block_size);
        (start, (end - start).min(self.max))
    }
}

impl Device for BlockDevice {
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < buf.len() {
            let at = offset + done as u64;
            let (start, len) = self.span(at, (buf.len() - done) as u64);
            self.read_blocks(start, len)?;
            let from = (at - start) as usize;
            let n = (len as usize - from).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&self.buffer()[from..from + n]);
            done += n;
        }
        Ok(())
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < data.len() {
            let at = offset + done as u64;
            let (start, len) = self.span(at, (data.len() - done) as u64);
            let from = (at - start) as usize;
            let n = (len as usize - from).min(data.len() - done);
            // Blocks that the write covers only in part are read first.
            if from != 0 || n != len as usize {
                self.read_blocks(start, len)?;
            }
            self.buffer()[from..from + n].copy_from_slice(&data[done..done + n]);
            self.write_blocks(start, len)?;
            done += n;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        match self.block.flush(FOREVER) {
            Ok(Ok(_)) => Ok(()),
            _ => Err(Error::Io),
        }
    }
}
