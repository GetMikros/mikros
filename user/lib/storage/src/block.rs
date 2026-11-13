//! The `Block` protocol on one connection: its buffers, its reads,
//! writes, flushes and discards, and on `bus` connections the writable
//! lock.

use crate::memory::BELOW_4G;
use crate::{Disk, PAGE, Writer, pages, pinned_with};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{
    Block, BlockAsyncServer, BlockCreateBufferResponse, BlockDiscardResponse, BlockFlags,
    BlockFlushResponse, BlockGetInfoResponse, BlockLockWritableResponse, BlockReadResponse,
    BlockRequest, BlockWriteResponse,
};
use mikros_rt::sys;

/// The most bytes of buffers one connection other than a `bus`
/// connection may have.
const BUFFERS_MAX: u64 = 64 << 20;
/// The rights of the buffer handles that clients get: DUPLICATE,
/// TRANSFER, READ, WRITE, MAP and INSPECT.
const CLIENT_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 2 | 1 << 3 | 1 << 5 | 1 << 9;

/// A buffer: its size, its memory object, and for a disk with a bus
/// transaction initiator the physical addresses of its pages and its
/// pin, which is unpinned when the buffer goes.
struct Buffer {
    size: u64,
    pages: Vec<u64>,
    mem: OwnedHandle,
    pmt: Option<sys::Handle>,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if let Some(pmt) = self.pmt {
            // SAFETY: the call takes no pointers; it also closes the
            // handle, which the buffer owns.
            unsafe { sys::pmt_unpin(pmt) };
        }
    }
}

/// How a connection was made: as a `bus` channel, or with `Open`, which
/// may write when its flag is true.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Bus,
    Open(bool),
}

/// A connection's buffers by ID, their total size, the ID the next one
/// gets, and whether it holds the disk's writable lock.
#[derive(Default)]
struct Connection {
    buffers: RefCell<BTreeMap<u32, Rc<Buffer>>>,
    total: Cell<u64>,
    next: Cell<u32>,
    locked: Cell<bool>,
}

/// Serves `Block` for `disk` on its `bus` channel `end` until it closes.
/// The connection may write, its buffers have no limit, and its writable
/// lock counts in `writer`, the disk's.
pub async fn serve_bus<D: Disk>(
    disk: Rc<D>,
    end: OwnedHandle,
    writer: Writer,
    reactor: Reactor,
    spawner: Spawner,
) {
    serve(disk, end, Access::Bus, writer, reactor, spawner, || {}).await;
}

/// Serves `Block` for `disk` on the channel `end`, made with `access`,
/// until it closes, and then calls `closed`. Each request runs as a task
/// of its own, so that many may be outstanding. Writes and discards fail
/// with `ACCESS_DENIED` on a read-only connection or disk.
/// `LockWritable` takes `writer`'s lock on a `bus` connection, and fails
/// with `ACCESS_DENIED` on any other.
pub(crate) async fn serve<D: Disk>(
    disk: Rc<D>,
    end: OwnedHandle,
    access: Access,
    writer: Writer,
    reactor: Reactor,
    spawner: Spawner,
    closed: impl FnOnce(),
) {
    use BlockRequest as R;
    let server = Rc::new(BlockAsyncServer::new(ServerEnd::<Block>::new(end), reactor));
    let connection = Rc::new(Connection::default());
    let writable = access != Access::Open(false);
    while let Ok(request) = server.next().await {
        let (disk, server, connection) = (disk.clone(), server.clone(), connection.clone());
        let writer = writer.clone();
        spawner.spawn(async move {
            let s = server.server();
            let read_only = disk.info().flags.0 & BlockFlags::READ_ONLY.0 != 0;
            let may_write = writable && !read_only;
            let limited = access != Access::Bus;
            let _ = match request {
                R::GetInfo(_, r) => r.reply(s, BlockGetInfoResponse { info: disk.info() }),
                R::CreateBuffer(q, r) => {
                    r.reply(s, create_buffer(&*disk, &connection, q.size, limited))
                }
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
                R::LockWritable(_, r) => {
                    let locked = match access {
                        Access::Open(_) => Err(Status::AccessDenied),
                        Access::Bus if connection.locked.get() => Ok(()),
                        Access::Bus if writer.lock() => {
                            connection.locked.set(true);
                            Ok(())
                        }
                        Access::Bus => Err(Status::AlreadyExists),
                    };
                    r.reply(s, locked.map(|()| BlockLockWritableResponse {}))
                }
                R::UnlockWritable(_) => {
                    if connection.locked.replace(false) {
                        writer.unlock();
                    }
                    Ok(())
                }
            };
        });
    }
    if connection.locked.replace(false) {
        writer.unlock();
    }
    closed();
}

/// A new buffer of `size` bytes, rounded up to a page, for `connection`,
/// pinned for the device, and a handle to it that the client can map.
/// Fails with `NO_RESOURCES` when the connection is `limited` and its
/// buffers would exceed 64 MiB.
fn create_buffer(
    disk: &impl Disk,
    connection: &Connection,
    size: u64,
    limited: bool,
) -> Result<BlockCreateBufferResponse, Status> {
    let size = size
        .checked_next_multiple_of(PAGE)
        .ok_or(Status::NoResources)?;
    if size == 0 {
        return Err(Status::InvalidArgs);
    }
    let total = connection.total.get() + size;
    if limited && total > BUFFERS_MAX {
        return Err(Status::NoResources);
    }
    let buffer = match disk.bti() {
        Some(bti) => {
            let (mem, pmt) = pinned_with(disk.dma(), bti, size, disk.below_4g())?;
            Buffer {
                size,
                pages: pages(&mem, size / PAGE)?,
                mem,
                pmt: Some(pmt.into_raw()),
            }
        }
        None => Buffer {
            size,
            pages: Vec::new(),
            mem: unpinned_buffer(disk.dma(), size, disk.below_4g())?,
            pmt: None,
        },
    };
    let mem = &buffer.mem;
    let memory = duplicate(mem, CLIENT_RIGHTS)?;
    let id = connection.next.get();
    connection.next.set(id.wrapping_add(1));
    connection.buffers.borrow_mut().insert(id, Rc::new(buffer));
    connection.total.set(total);
    Ok(BlockCreateBufferResponse { buffer: id, memory })
}

/// Reads or writes `count` blocks from `block` at `offset` in buffer
/// `id`, after checking the request: `INVALID_ARGS` for no blocks, more
/// than `max_transfer_blocks`, an `offset` that is not a multiple of 4 or
/// an unknown buffer, and `OUT_OF_RANGE` for blocks beyond the disk or
/// bytes beyond the buffer.
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
    if count == 0 || count > info.max_transfer_blocks || !offset.is_multiple_of(4) {
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
    match disk.bti() {
        Some(_) => {
            disk.transfer(write, block, &buffer.pages, offset, len, fua)
                .await
        }
        None => {
            disk.transfer_object(write, block, &buffer.mem, offset, len, fua)
                .await
        }
    }
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

/// A pinned memory object of `size` bytes from the `DMA` resource
/// `dma`, below 4 GiB when `below_4g` is true, for a disk without a bus
/// transaction initiator.
fn unpinned_buffer(dma: &OwnedHandle, size: u64, below_4g: bool) -> Result<OwnedHandle, Status> {
    let options = if below_4g { BELOW_4G } else { 0 };
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create_pinned(dma.raw(), size, options, &mut mem) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(mem) })
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
