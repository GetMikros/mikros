//! `Block` for one partition, forwarded to the disk's `Block` on the bus
//! channel through the link, which keeps requests across the disk
//! driver's restarts: block numbers are moved by the partition's first
//! block, and buffers are the disk's own, under numbers of the link's.

use crate::link::Link;
use crate::writers::Writers;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{
    Block, BlockAsyncServer, BlockCreateBufferResponse, BlockDiscardResponse, BlockFlags,
    BlockFlushResponse, BlockGetInfoResponse, BlockInfo, BlockLockWritableResponse,
    BlockReadResponse, BlockRequest, BlockWriteResponse,
};

/// The most bytes of buffers one connection other than a `bus`
/// connection may have.
const BUFFERS_MAX: u64 = 64 << 20;
const PAGE: u64 = 4096;

/// A partition: its first block on the disk, and what its `GetInfo`
/// reports, with the disk's values but its own block count and serial.
pub struct Partition {
    pub first: u64,
    pub info: BlockInfo,
}

/// How a connection was made: as a partition's `bus` channel, or with
/// `Open`, which may write when its flag is true.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Bus,
    Open(bool),
}

/// The disk's buffers that a connection created, with their sizes,
/// their total size, and whether it holds a writable lock.
#[derive(Default)]
struct Connection {
    buffers: RefCell<BTreeMap<u32, u64>>,
    total: Cell<u64>,
    locked: Cell<bool>,
    locking: Cell<bool>,
}

/// Serves `Block` for `partition` on `end`, made with `access`, until it
/// closes. Then destroys the buffers the connection left, releases its
/// lock and calls `closed`. Each request runs as a task of its own.
/// `LockWritable` takes a lock in `writers` on a
/// `bus` connection, and fails with `ACCESS_DENIED` on any other.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    disk: Rc<Link>,
    partition: Rc<Partition>,
    end: OwnedHandle,
    access: Access,
    writers: Rc<Writers>,
    reactor: Reactor,
    spawner: Spawner,
    closed: impl FnOnce(),
) {
    use BlockRequest as R;
    let server = Rc::new(BlockAsyncServer::new(ServerEnd::<Block>::new(end), reactor));
    let connection = Rc::new(Connection::default());
    let info = &partition.info;
    let read_only = info.flags.0 & BlockFlags::READ_ONLY.0 != 0;
    let may_write = access != Access::Open(false) && !read_only;
    let limited = access != Access::Bus;
    while let Ok(request) = server.next().await {
        let (disk, partition, writers) = (disk.clone(), partition.clone(), writers.clone());
        let (server, connection) = (server.clone(), connection.clone());
        spawner.spawn(async move {
            let s = server.server();
            let p = &*partition;
            let _ = match request {
                R::GetInfo(_, r) => r.reply(
                    s,
                    BlockGetInfoResponse {
                        info: copy(&p.info),
                    },
                ),
                R::CreateBuffer(q, r) => {
                    r.reply(s, create_buffer(&disk, &connection, q.size, limited).await)
                }
                R::DestroyBuffer(q) => {
                    if let Some(size) = connection.buffers.borrow_mut().remove(&q.buffer) {
                        connection.total.set(connection.total.get() - size);
                        disk.destroy_buffer(q.buffer);
                    }
                    Ok(())
                }
                R::Read(q, r) => {
                    let read = match check(p, &connection, q.buffer, q.block, q.count) {
                        Err(status) => Err(status),
                        Ok(block) => disk.read(q.buffer, q.offset, block, q.count).await,
                    };
                    r.reply(s, read.map(|_| BlockReadResponse {}))
                }
                R::Write(q, r) => {
                    let written = match check(p, &connection, q.buffer, q.block, q.count) {
                        _ if !may_write => Err(Status::AccessDenied),
                        Err(status) => Err(status),
                        Ok(block) => disk.write(q.buffer, q.offset, block, q.count, q.fua).await,
                    };
                    r.reply(s, written.map(|_| BlockWriteResponse {}))
                }
                R::Flush(_, r) => r.reply(s, disk.flush().await.map(|_| BlockFlushResponse {})),
                R::Discard(q, r) => {
                    let discarded = match discard_range(p, may_write, q.block, q.count) {
                        Err(status) => Err(status),
                        Ok(block) => disk.discard(block, q.count).await,
                    };
                    r.reply(s, discarded.map(|_| BlockDiscardResponse {}))
                }
                R::LockWritable(_, r) => {
                    let locked = match access {
                        Access::Open(_) => Err(Status::AccessDenied),
                        Access::Bus => lock(&connection, &writers).await,
                    };
                    r.reply(s, locked.map(|()| BlockLockWritableResponse {}))
                }
                R::UnlockWritable(_) => {
                    if connection.locked.replace(false) {
                        writers.unlock();
                    }
                    Ok(())
                }
            };
        });
    }
    for (buffer, _) in connection.buffers.take() {
        disk.destroy_buffer(buffer);
    }
    if connection.locked.replace(false) {
        writers.unlock();
    }
    closed();
}

/// Takes a writable lock for the `bus` connection `connection`, which
/// succeeds at once when it holds one. `ALREADY_EXISTS` when `writers`
/// refuses it, or while another request of the connection takes it.
async fn lock(connection: &Connection, writers: &Writers) -> Result<(), Status> {
    if connection.locked.get() {
        return Ok(());
    }
    if connection.locking.replace(true) {
        return Err(Status::AlreadyExists);
    }
    let locked = writers.lock().await;
    connection.locking.set(false);
    connection.locked.set(locked);
    locked.then_some(()).ok_or(Status::AlreadyExists)
}

/// A buffer of `size` bytes from the disk, rounded up to a page, owned by
/// `connection`. Fails with `NO_RESOURCES` when the connection is
/// `limited` and its buffers would exceed 64 MiB, and otherwise as the
/// disk's `CreateBuffer` does.
async fn create_buffer(
    disk: &Rc<Link>,
    connection: &Connection,
    size: u64,
    limited: bool,
) -> Result<BlockCreateBufferResponse, Status> {
    let rounded = size
        .checked_next_multiple_of(PAGE)
        .ok_or(Status::NoResources)?;
    let over = |total: u64| limited && total + rounded > BUFFERS_MAX;
    if over(connection.total.get()) {
        return Err(Status::NoResources);
    }
    let created = disk.create_buffer(size).await?;
    // The total is checked again: other requests ran while this one waited.
    if over(connection.total.get()) {
        disk.destroy_buffer(created.buffer);
        return Err(Status::NoResources);
    }
    connection
        .buffers
        .borrow_mut()
        .insert(created.buffer, rounded);
    connection.total.set(connection.total.get() + rounded);
    Ok(created)
}

/// The disk block of a read or write of `count` blocks from `block` with
/// buffer `id`: `INVALID_ARGS` for no blocks or a buffer that the
/// connection did not create, and `OUT_OF_RANGE` for blocks beyond the
/// partition. The disk checks the rest.
fn check(
    partition: &Partition,
    connection: &Connection,
    id: u32,
    block: u64,
    count: u32,
) -> Result<u64, Status> {
    if count == 0 || !connection.buffers.borrow().contains_key(&id) {
        return Err(Status::InvalidArgs);
    }
    within(partition, block, u64::from(count))
}

/// The disk block of a discard of `count` blocks from `block`, on a
/// connection and partition that may write when `may_write` is true.
fn discard_range(
    partition: &Partition,
    may_write: bool,
    block: u64,
    count: u64,
) -> Result<u64, Status> {
    if !may_write {
        return Err(Status::AccessDenied);
    }
    if partition.info.flags.0 & BlockFlags::DISCARD.0 == 0 {
        return Err(Status::NotSupported);
    }
    if count == 0 {
        return Err(Status::InvalidArgs);
    }
    within(partition, block, count)
}

/// The disk block of `block`, when the `count` blocks from it lie in the
/// partition, and otherwise `OUT_OF_RANGE`.
fn within(partition: &Partition, block: u64, count: u64) -> Result<u64, Status> {
    match block.checked_add(count) {
        Some(end) if end <= partition.info.block_count => Ok(partition.first + block),
        _ => Err(Status::OutOfRange),
    }
}

/// A copy of `info`.
pub fn copy(info: &BlockInfo) -> BlockInfo {
    BlockInfo {
        block_size: info.block_size,
        block_count: info.block_count,
        max_transfer_blocks: info.max_transfer_blocks,
        flags: info.flags,
        model: info.model.clone(),
        serial: info.serial.clone(),
    }
}
