//! The `Block` protocol on one connection: its buffers, and its reads,
//! writes, flushes and discards.

use crate::{Disk, PAGE, pages, pinned_with};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{
    Block, BlockAsyncServer, BlockCreateBufferResponse, BlockDiscardResponse, BlockFlags,
    BlockFlushResponse, BlockGetInfoResponse, BlockReadResponse, BlockRequest, BlockWriteResponse,
};
use mikros_rt::sys;

/// The most bytes of buffers one connection may have.
const BUFFERS_MAX: u64 = 64 << 20;
/// The rights of the buffer handles that clients get: DUPLICATE,
/// TRANSFER, READ, WRITE, MAP and INSPECT.
const CLIENT_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 2 | 1 << 3 | 1 << 5 | 1 << 9;

/// A buffer: its size, the physical addresses of its pages, its memory
/// object, and its pin, which is unpinned when the buffer goes.
struct Buffer {
    size: u64,
    pages: Vec<u64>,
    _mem: OwnedHandle,
    pmt: sys::Handle,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: the call takes no pointers; it also closes the handle,
        // which the buffer owns.
        unsafe { sys::pmt_unpin(self.pmt) };
    }
}

/// A connection's buffers by ID, their total size, and the ID the next
/// one gets.
#[derive(Default)]
struct Connection {
    buffers: RefCell<BTreeMap<u32, Rc<Buffer>>>,
    total: Cell<u64>,
    next: Cell<u32>,
}

/// Serves `Block` for `disk` on the channel `end`, which may write when
/// `writable` is true, until it closes, and then calls `closed`. Each
/// request runs as a task of its own, so that many may be outstanding.
/// Writes and discards fail with `ACCESS_DENIED` on a read-only
/// connection or disk.
pub async fn serve<D: Disk>(
    disk: Rc<D>,
    end: OwnedHandle,
    writable: bool,
    reactor: Reactor,
    spawner: Spawner,
    closed: impl FnOnce(),
) {
    use BlockRequest as R;
    let server = Rc::new(BlockAsyncServer::new(ServerEnd::<Block>::new(end), reactor));
    let connection = Rc::new(Connection::default());
    while let Ok(request) = server.next().await {
        let (disk, server, connection) = (disk.clone(), server.clone(), connection.clone());
        spawner.spawn(async move {
            let s = server.server();
            let read_only = disk.info().flags.0 & BlockFlags::READ_ONLY.0 != 0;
            let may_write = writable && !read_only;
            let _ = match request {
                R::GetInfo(_, r) => r.reply(s, BlockGetInfoResponse { info: disk.info() }),
                R::CreateBuffer(q, r) => r.reply(s, create_buffer(&*disk, &connection, q.size)),
                R::DestroyBuffer(q) => {
                    if let Some(buffer) = connection.buffers.borrow_mut().remove(&q.buffer) {
                        connection.total.set(connection.total.get() - buffer.size);
                    }
                    Ok(())
                }
                R::Read(q, r) => {
                    let read = transfer(
                        &*disk,
                        &connection,
                        false,
                        q.buffer,
                        q.offset,
                        q.block,
                        q.count,
                        false,
                    );
                    r.reply(s, read.await.map(|()| BlockReadResponse {}))
                }
                R::Write(q, r) => {
                    let written = match may_write {
                        false => Err(Status::AccessDenied),
                        true => {
                            let t = transfer(
                                &*disk,
                                &connection,
                                true,
                                q.buffer,
                                q.offset,
                                q.block,
                                q.count,
                                q.fua,
                            );
                            t.await
                        }
                    };
                    r.reply(s, written.map(|()| BlockWriteResponse {}))
                }
                R::Flush(_, r) => r.reply(s, disk.flush().await.map(|()| BlockFlushResponse {})),
                R::Discard(q, r) => {
                    let discarded = discard(&*disk, may_write, q.block, q.count).await;
                    r.reply(s, discarded.map(|()| BlockDiscardResponse {}))
                }
            };
        });
    }
    closed();
}

/// A new buffer of `size` bytes, rounded up to a page, for `connection`,
/// pinned for the device, and a handle to it that the client can map.
/// Fails with `NO_RESOURCES` when the connection's buffers would exceed
/// 64 MiB.
fn create_buffer(
    disk: &impl Disk,
    connection: &Connection,
    size: u64,
) -> Result<BlockCreateBufferResponse, Status> {
    let size = size
        .checked_next_multiple_of(PAGE)
        .ok_or(Status::NoResources)?;
    if size == 0 {
        return Err(Status::InvalidArgs);
    }
    let total = connection.total.get() + size;
    if total > BUFFERS_MAX {
        return Err(Status::NoResources);
    }
    let (mem, pmt) = pinned_with(disk.dma(), disk.bti(), size, disk.below_4g())?;
    let buffer = Buffer {
        size,
        pages: pages(&mem, size / PAGE)?,
        _mem: duplicate(&mem, 1 << 31)?,
        pmt: pmt.into_raw(),
    };
    let memory = duplicate(&mem, CLIENT_RIGHTS)?;
    let id = connection.next.get();
    connection.next.set(id.wrapping_add(1));
    connection.buffers.borrow_mut().insert(id, Rc::new(buffer));
    connection.total.set(total);
    Ok(BlockCreateBufferResponse { buffer: id, memory })
}

/// Reads or writes `count` blocks from `block` at `offset` in buffer
/// `id`, after checking the request: `INVALID_ARGS` for no blocks, more
/// than `max_transfer_blocks` or an unknown buffer, and `OUT_OF_RANGE`
/// for blocks beyond the disk or bytes beyond the buffer.
#[allow(clippy::too_many_arguments)]
async fn transfer(
    disk: &impl Disk,
    connection: &Connection,
    write: bool,
    id: u32,
    offset: u64,
    block: u64,
    count: u32,
    fua: bool,
) -> Result<(), Status> {
    let info = disk.info();
    if count == 0 || count > info.max_transfer_blocks {
        return Err(Status::InvalidArgs);
    }
    let buffer = connection
        .buffers
        .borrow()
        .get(&id)
        .cloned()
        .ok_or(Status::InvalidArgs)?;
    let len = u64::from(count) * u64::from(info.block_size);
    let beyond_disk = block
        .checked_add(u64::from(count))
        .is_none_or(|end| end > info.block_count);
    let beyond_buffer = offset.checked_add(len).is_none_or(|end| end > buffer.size);
    if beyond_disk || beyond_buffer {
        return Err(Status::OutOfRange);
    }
    disk.transfer(write, block, &buffer.pages, offset, len, fua)
        .await
}

/// Discards `count` blocks from `block`, on a connection and disk that
/// may write when `may_write` is true: `NOT_SUPPORTED` when the disk does
/// not report `DISCARD`.
async fn discard(disk: &impl Disk, may_write: bool, block: u64, count: u64) -> Result<(), Status> {
    if !may_write {
        return Err(Status::AccessDenied);
    }
    let info = disk.info();
    if info.flags.0 & BlockFlags::DISCARD.0 == 0 {
        return Err(Status::NotSupported);
    }
    if count == 0 {
        return Err(Status::InvalidArgs);
    }
    if block
        .checked_add(count)
        .is_none_or(|end| end > info.block_count)
    {
        return Err(Status::OutOfRange);
    }
    disk.discard(block, count).await
}

fn duplicate(handle: &OwnedHandle, rights: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), rights, &mut out) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}
