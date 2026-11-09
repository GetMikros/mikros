//! As the durability test driver: on the partition of each scratch disk
//! of the durability run, six blocks written one after the other, which
//! the runner finds in QEMU's log of the writes and flushes that reach
//! the disk. Block 1 is written and then flushed, block 3 is written with
//! `fua`, and block 5 is written plainly. Blocks 2, 4 and 6, each written
//! once the block before it, and its flush, completed, mark where those
//! completed in the log. Each block starts with `MAGIC` and its number,
//! and block `n` is block `n - 1` of the partition.

use crate::{PAGE_SIZE, PERM_READ, PERM_WRITE};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::DriverStartResponse;
use mikros_rt::interfaces::mikros::storage::BlockAsyncClient;
use mikros_rt::sys;

/// The GUIDs of the partitions of the scratch disks, behind NVMe, AHCI,
/// USB mass storage and virtio-blk.
const SCRATCH: [&str; 4] = [
    "4e564d65-0000-4000-8000-000000000001",
    "41484349-0000-4000-8000-000000000002",
    "55534221-0000-4000-8000-000000000003",
    "56495254-0000-4000-8000-000000000004",
];
const MAGIC: &[u8; 8] = b"MKDURABL";
const BLOCKS: u32 = 6;
/// The block written with `fua`, and the one flushed after its write.
const FUA: u32 = 3;
const FLUSHED: u32 = 1;

/// Writes the blocks on the partition behind the bus channel `bus`, and
/// logs that it did, with the disk's model. Declines a partition that is
/// not a scratch disk's with `NOT_SUPPORTED`.
pub async fn works(
    bus: OwnedHandle,
    reactor: &Reactor,
    process: sys::Handle,
) -> Result<DriverStartResponse, Status> {
    let disk = BlockAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let info = disk.get_info().await.map_err(|_| Status::PeerClosed)?.info;
    if !SCRATCH.contains(&info.serial.as_str()) {
        return Err(Status::NotSupported);
    }
    let failed = |what: &str| {
        mikros_rt::log!(Error, "test-durable: FAILED: {what} on {}", info.model);
        Err(Status::InvalidArgs)
    };
    let size = u64::from(info.block_size);
    let Ok(Ok(created)) = disk.create_buffer(PAGE_SIZE).await else {
        return failed("creating a buffer");
    };
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let mapped = unsafe {
        sys::vm_map(
            process,
            PERM_READ | PERM_WRITE,
            0,
            created.memory.raw(),
            0,
            PAGE_SIZE,
            &mut at,
        )
    };
    if mapped != mikros_rt::OK || size > PAGE_SIZE {
        return failed("mapping the buffer");
    }
    // SAFETY: the buffer's first page is mapped at `at`, and only this
    // driver writes it.
    let block = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, size as usize) };
    for n in 1..=BLOCKS {
        for (i, byte) in block.iter_mut().enumerate() {
            *byte = (i as u32 ^ n) as u8;
        }
        block[..8].copy_from_slice(MAGIC);
        block[8..12].copy_from_slice(&n.to_le_bytes());
        let lba = u64::from(n - 1);
        if !matches!(
            disk.write(created.buffer, 0, lba, 1, n == FUA).await,
            Ok(Ok(_))
        ) {
            return failed("writing a block");
        }
        if n == FLUSHED && !matches!(disk.flush().await, Ok(Ok(_))) {
            return failed("flushing");
        }
    }
    mikros_rt::log!(Info, "test-durable: wrote the blocks on {}", info.model);
    Ok(DriverStartResponse {})
}
