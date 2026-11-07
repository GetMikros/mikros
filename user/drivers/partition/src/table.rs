//! Reading a disk's GUID partition table through its `Block` channel.

use alloc::vec::Vec;
use mikros_gpt::{Header, Partition};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockAsyncClient, BlockInfo};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PAGE: u64 = 4096;
/// The largest partition entry array the driver reads.
const ENTRIES_MAX: u64 = 1 << 20;

/// Which table the partitions came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    Primary,
    Backup,
}

/// A disk buffer mapped into the driver.
struct Mapped<'a> {
    disk: &'a BlockAsyncClient,
    process: sys::Handle,
    id: u32,
    at: u64,
    size: u64,
}

impl<'a> Mapped<'a> {
    async fn new(
        disk: &'a BlockAsyncClient,
        process: sys::Handle,
        size: u64,
    ) -> Result<Mapped<'a>, Status> {
        let created = match disk.create_buffer(size).await {
            Ok(Ok(created)) => created,
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        };
        let size = size.next_multiple_of(PAGE);
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let status = unsafe {
            sys::vm_map(
                process,
                PERM_READ,
                0,
                created.memory.raw(),
                0,
                size,
                &mut at,
            )
        };
        if status != mikros_rt::OK {
            let _ = disk.destroy_buffer(created.buffer);
            return Err(Status::from_raw(status as i32));
        }
        Ok(Mapped {
            disk,
            process,
            id: created.buffer,
            at,
            size,
        })
    }

    /// Reads `bytes` bytes from block `block` into the start of the
    /// buffer, in requests of at most `max` blocks of `block_size`
    /// bytes, and returns them.
    async fn read(
        &self,
        block: u64,
        bytes: u64,
        block_size: u32,
        max: u32,
    ) -> Result<&[u8], Status> {
        let count = bytes.div_ceil(u64::from(block_size));
        let mut done = 0;
        while done < count {
            let step = (count - done).min(u64::from(max));
            let offset = done * u64::from(block_size);
            match self
                .disk
                .read(self.id, offset, block + done, step as u32)
                .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(status)) => return Err(status),
                Err(_) => return Err(Status::PeerClosed),
            }
            done += step;
        }
        // SAFETY: the buffer is mapped at `at` for `size` bytes, and the
        // reads above filled the bytes asked for.
        Ok(unsafe { core::slice::from_raw_parts(self.at as *const u8, bytes as usize) })
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        // SAFETY: nothing refers to the mapping any more.
        unsafe { sys::vm_unmap(self.process, self.at, self.size) };
        let _ = self.disk.destroy_buffer(self.id);
    }
}

/// The partitions of the disk behind `disk`, which `info` describes, and
/// the table they came from: the primary table at LBA 1 when its header
/// and entry array check, and otherwise the backup table at the last
/// LBA when it does. None when neither is valid.
pub async fn read(
    disk: &BlockAsyncClient,
    info: &BlockInfo,
    process: sys::Handle,
) -> Option<(Table, Vec<Partition>)> {
    let last = info.block_count.checked_sub(1)?;
    for (table, lba) in [(Table::Primary, 1), (Table::Backup, last)] {
        if let Some(partitions) = read_one(disk, info, process, lba).await {
            return Some((table, partitions));
        }
    }
    None
}

/// The partitions of the table whose header is at `lba`, when it checks.
async fn read_one(
    disk: &BlockAsyncClient,
    info: &BlockInfo,
    process: sys::Handle,
    lba: u64,
) -> Option<Vec<Partition>> {
    let (size, max) = (info.block_size, info.max_transfer_blocks.max(1));
    let header: Header = {
        let buffer = Mapped::new(disk, process, u64::from(size)).await.ok()?;
        let block = buffer.read(lba, u64::from(size), size, max).await.ok()?;
        mikros_gpt::header(block, lba)?
    };
    let bytes = header.entries_size();
    let blocks = bytes.div_ceil(u64::from(size));
    let fits = header
        .entries_lba
        .checked_add(blocks)
        .is_some_and(|end| end <= info.block_count);
    if bytes == 0 || bytes > ENTRIES_MAX || !fits {
        return None;
    }
    let buffer = Mapped::new(disk, process, blocks * u64::from(size))
        .await
        .ok()?;
    let entries = buffer
        .read(header.entries_lba, bytes, size, max)
        .await
        .ok()?;
    let partitions = header.partitions(entries)?;
    // Entries that do not lie on the disk are left out.
    let on_disk = |p: &Partition| p.first_lba <= p.last_lba && p.last_lba < info.block_count;
    Some(partitions.into_iter().filter(on_disk).collect())
}
