//! The disk's `Block` connection, the driver's `bus` channel, which
//! outlives the disk driver (MIKSPEC-0044.R19). When the channel closes
//! because the disk driver terminated, the requests that had not
//! completed wait. `Reconnect` (MIKSPEC-0025.R34) gives a new channel, on
//! which the link creates the buffers again and takes the writable lock
//! again if it was held; then it sends those requests again one at a
//! time, in the order in which they first came, before any other. When
//! no channel comes within 10 seconds, or the driver stops, they fail
//! with `IO`.
//!
//! The link numbers buffers itself, so that their numbers stay when the
//! disk's change. A buffer that a client created before the disk driver
//! restarted is the old disk's memory, which the client keeps: before
//! each write the link copies what is written from it into the new
//! disk's buffer, and after each read what was read back into it.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::{Future, poll_fn};
use core::task::{Poll, Waker};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, Error, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockAsyncClient, BlockCreateBufferResponse};
use mikros_rt::sys;

/// How long requests wait for a new channel.
const RECONNECT_LIMIT: i64 = 10_000_000_000;
const PAGE: u64 = 4096;
const PERM_READ_WRITE: u32 = (1 << 0) | (1 << 1);
const SAME_RIGHTS: u32 = 1 << 31;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Up,
    /// The channel closed, or a new one is being set up.
    Down,
    /// No channel came in time, or the driver stopped.
    Failed,
}

/// A buffer that a client created through the link.
struct Buffer {
    size: u64,
    /// The memory that the client got, and where it is mapped here once
    /// the disk's buffer is other memory.
    memory: OwnedHandle,
    client: Option<u64>,
    /// The disk's buffer on the current channel, and its memory mapped
    /// here when it is not `memory`.
    disk: u32,
    copy: Option<(OwnedHandle, u64)>,
}

pub struct Link {
    process: sys::Handle,
    block_size: u64,
    reactor: Reactor,
    spawner: Spawner,
    disk: RefCell<Rc<BlockAsyncClient>>,
    /// Counted up by each new channel.
    generation: Cell<u64>,
    state: Cell<State>,
    /// The number of the next request, the requests sent and not
    /// completed, and those to send again before any other.
    next: Cell<u64>,
    unfinished: RefCell<BTreeSet<u64>>,
    replay: RefCell<VecDeque<u64>>,
    buffers: RefCell<BTreeMap<u32, Buffer>>,
    next_buffer: Cell<u32>,
    /// Whether the writable lock on the disk is held.
    locked: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
}

/// The result of a call to the disk, with `PEER_CLOSED` when its channel
/// failed.
fn flatten<T>(result: Result<Result<T, Status>, Error>) -> Result<T, Status> {
    result.unwrap_or(Err(Status::PeerClosed))
}

fn duplicate(handle: &OwnedHandle) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    match unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) } {
        // SAFETY: the kernel just made the handle for us.
        mikros_rt::OK => Ok(unsafe { OwnedHandle::from_raw(out) }),
        _ => Err(Status::NoResources),
    }
}

impl Link {
    pub fn new(
        disk: Rc<BlockAsyncClient>,
        block_size: u64,
        process: sys::Handle,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Rc<Link> {
        Rc::new(Link {
            process,
            block_size,
            reactor,
            spawner,
            disk: RefCell::new(disk),
            generation: Cell::new(0),
            state: Cell::new(State::Up),
            next: Cell::new(0),
            unfinished: RefCell::new(BTreeSet::new()),
            replay: RefCell::new(VecDeque::new()),
            buffers: RefCell::new(BTreeMap::new()),
            next_buffer: Cell::new(1),
            locked: Cell::new(false),
            waiting: RefCell::new(Vec::new()),
        })
    }

    fn wake(&self) {
        let waiting: Vec<Waker> = self.waiting.borrow_mut().drain(..).collect();
        for waker in waiting {
            waker.wake();
        }
    }

    /// Waits until request `number` may be sent: not while the channel
    /// is down, nor while requests are sent again, unless it is the next
    /// of those. `IO` once the link failed.
    async fn turn(&self, number: u64) -> Result<(), Status> {
        poll_fn(|cx| {
            match self.state.get() {
                State::Failed => return Poll::Ready(Err(Status::Io)),
                State::Up => match self.replay.borrow().front() {
                    None => return Poll::Ready(Ok(())),
                    Some(&first) if first == number => return Poll::Ready(Ok(())),
                    Some(_) => {}
                },
                State::Down => {}
            }
            self.waiting.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// Ends request `number`, which lets the next to send again go. A
    /// request that completed on the old channel after a new one came
    /// is not sent again.
    fn finish(&self, number: u64) {
        self.unfinished.borrow_mut().remove(&number);
        self.replay.borrow_mut().retain(|&n| n != number);
        self.wake();
    }

    /// Takes the link down when the channel of `generation` closed.
    fn lost(self: &Rc<Self>, generation: u64) {
        if generation != self.generation.get() || self.state.get() != State::Up {
            return;
        }
        self.state.set(State::Down);
        self.expire();
    }

    /// Fails the link unless a new channel is up within
    /// `RECONNECT_LIMIT`.
    fn expire(self: &Rc<Self>) {
        let generation = self.generation.get();
        let link = self.clone();
        self.spawner.spawn(async move {
            let mut now = 0;
            // SAFETY: the out pointer is valid for the call.
            unsafe { sys::clock_get_monotonic(&mut now) };
            let _ = link.reactor.sleep_until(now + RECONNECT_LIMIT).await;
            if link.generation.get() == generation && link.state.get() == State::Down {
                link.fail();
            }
        });
    }

    /// Fails the requests that wait, and all later ones, with `IO`.
    pub fn fail(&self) {
        self.state.set(State::Failed);
        self.wake();
    }

    /// Makes a request with `attempt`, which gets the disk's channel of
    /// the moment, and makes it again on a new channel when the old one
    /// closes before it completes.
    async fn request<T, F, R>(self: &Rc<Self>, attempt: F) -> Result<T, Status>
    where
        F: Fn(Rc<BlockAsyncClient>) -> R,
        R: Future<Output = Result<Result<T, Status>, Error>>,
    {
        let number = self.next.get();
        self.next.set(number + 1);
        loop {
            if let Err(status) = self.turn(number).await {
                self.finish(number);
                return Err(status);
            }
            let disk = self.disk.borrow().clone();
            let generation = self.generation.get();
            self.unfinished.borrow_mut().insert(number);
            match attempt(disk).await {
                Ok(result) => {
                    self.finish(number);
                    return result;
                }
                Err(_) => self.lost(generation),
            }
        }
    }

    /// Continues on `bus`, the channel that `Reconnect` gave: creates the
    /// buffers again, takes the writable lock again if it was held, and
    /// then lets the requests that had not completed go again, in order.
    /// Fails, staying down, when the new disk refuses a buffer or the
    /// lock.
    pub async fn reconnect(self: &Rc<Self>, bus: OwnedHandle) -> Result<(), Status> {
        self.state.set(State::Down);
        self.generation.set(self.generation.get() + 1);
        let set_up = self.set_up(bus).await;
        if set_up.is_err() {
            self.expire();
        }
        set_up
    }

    async fn set_up(self: &Rc<Self>, bus: OwnedHandle) -> Result<(), Status> {
        let disk = Rc::new(BlockAsyncClient::new(
            ClientEnd::new(bus),
            self.reactor.clone(),
        ));
        let ids: Vec<u32> = self.buffers.borrow().keys().copied().collect();
        for id in ids {
            let Some(size) = self.buffers.borrow().get(&id).map(|b| b.size) else {
                continue;
            };
            let created = flatten(disk.create_buffer(size).await)?;
            let view = self.map(&created.memory, size)?;
            let mut buffers = self.buffers.borrow_mut();
            // A buffer destroyed meanwhile goes on the new disk too.
            let Some(buffer) = buffers.get_mut(&id) else {
                drop(buffers);
                self.unmap(view, size);
                let _ = disk.destroy_buffer(created.buffer);
                continue;
            };
            if buffer.client.is_none() {
                buffer.client = Some(self.map(&buffer.memory, size)?);
            }
            if let Some((_, old)) = buffer.copy.take() {
                self.unmap(old, size);
            }
            buffer.disk = created.buffer;
            buffer.copy = Some((created.memory, view));
        }
        if self.locked.get() {
            flatten(disk.lock_writable().await)?;
        }
        *self.disk.borrow_mut() = disk;
        *self.replay.borrow_mut() = self.unfinished.borrow().iter().copied().collect();
        self.state.set(State::Up);
        self.wake();
        Ok(())
    }

    fn map(&self, memory: &OwnedHandle, size: u64) -> Result<u64, Status> {
        let mut at = 0;
        let len = size.next_multiple_of(PAGE);
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let mapped = unsafe {
            sys::vm_map(
                self.process,
                PERM_READ_WRITE,
                0,
                memory.raw(),
                0,
                len,
                &mut at,
            )
        };
        (mapped == mikros_rt::OK)
            .then_some(at)
            .ok_or(Status::NoResources)
    }

    fn unmap(&self, at: u64, size: u64) {
        // SAFETY: the call takes no pointers; the range is a mapping of
        // the link's.
        unsafe { sys::vm_unmap(self.process, at, size.next_multiple_of(PAGE)) };
    }

    /// A buffer of `size` bytes from the disk, with a number of the
    /// link's.
    pub async fn create_buffer(
        self: &Rc<Self>,
        size: u64,
    ) -> Result<BlockCreateBufferResponse, Status> {
        let created = self
            .request(|disk| async move { disk.create_buffer(size).await })
            .await?;
        let Ok(memory) = duplicate(&created.memory) else {
            let _ = self.disk.borrow().destroy_buffer(created.buffer);
            return Err(Status::NoResources);
        };
        let id = self.next_buffer.get();
        self.next_buffer.set(id + 1);
        self.buffers.borrow_mut().insert(
            id,
            Buffer {
                size,
                memory,
                client: None,
                disk: created.buffer,
                copy: None,
            },
        );
        Ok(BlockCreateBufferResponse {
            buffer: id,
            memory: created.memory,
        })
    }

    pub fn destroy_buffer(&self, id: u32) {
        let Some(buffer) = self.buffers.borrow_mut().remove(&id) else {
            return;
        };
        if let Some(client) = buffer.client {
            self.unmap(client, buffer.size);
        }
        if let Some((_, view)) = buffer.copy {
            self.unmap(view, buffer.size);
        }
        if self.state.get() == State::Up {
            let _ = self.disk.borrow().destroy_buffer(buffer.disk);
        }
    }

    /// The disk's buffer for buffer `id`, and, when it is other memory
    /// than the client's, where the client's and the disk's memory are
    /// mapped, for `len` bytes at `offset`: `INVALID_ARGS` for a buffer
    /// that does not exist or a range beyond it.
    fn disk_buffer(
        &self,
        id: u32,
        offset: u64,
        len: u64,
    ) -> Result<(u32, Option<(u64, u64)>), Status> {
        let buffers = self.buffers.borrow();
        let buffer = buffers.get(&id).ok_or(Status::InvalidArgs)?;
        let end = offset.checked_add(len).ok_or(Status::InvalidArgs)?;
        if end > buffer.size {
            return Err(Status::InvalidArgs);
        }
        let views = match (&buffer.copy, buffer.client) {
            (Some((_, disk)), Some(client)) => Some((client + offset, disk + offset)),
            _ => None,
        };
        Ok((buffer.disk, views))
    }

    pub async fn read(
        self: &Rc<Self>,
        id: u32,
        offset: u64,
        block: u64,
        count: u32,
    ) -> Result<(), Status> {
        let len = u64::from(count) * self.block_size;
        self.request(|disk| {
            let link = self.clone();
            async move {
                let (buffer, views) = match link.disk_buffer(id, offset, len) {
                    Ok(found) => found,
                    Err(status) => return Ok(Err(status)),
                };
                let read = disk.read(buffer, offset, block, count).await;
                if let (Ok(Ok(_)), Some((client, disk))) = (&read, views) {
                    // SAFETY: both ranges lie in mappings of the link's,
                    // which the disk filled.
                    unsafe { core::ptr::copy(disk as *const u8, client as *mut u8, len as usize) };
                }
                read
            }
        })
        .await
        .map(|_| ())
    }

    pub async fn write(
        self: &Rc<Self>,
        id: u32,
        offset: u64,
        block: u64,
        count: u32,
        fua: bool,
    ) -> Result<(), Status> {
        let len = u64::from(count) * self.block_size;
        self.request(|disk| {
            let link = self.clone();
            async move {
                let (buffer, views) = match link.disk_buffer(id, offset, len) {
                    Ok(found) => found,
                    Err(status) => return Ok(Err(status)),
                };
                if let Some((client, disk)) = views {
                    // SAFETY: both ranges lie in mappings of the link's.
                    unsafe { core::ptr::copy(client as *const u8, disk as *mut u8, len as usize) };
                }
                disk.write(buffer, offset, block, count, fua).await
            }
        })
        .await
        .map(|_| ())
    }

    pub async fn flush(self: &Rc<Self>) -> Result<(), Status> {
        self.request(|disk| async move { disk.flush().await })
            .await
            .map(|_| ())
    }

    pub async fn discard(self: &Rc<Self>, block: u64, count: u64) -> Result<(), Status> {
        self.request(|disk| async move { disk.discard(block, count).await })
            .await
            .map(|_| ())
    }

    /// Takes the disk's writable lock, which a new channel takes again.
    pub async fn lock_writable(self: &Rc<Self>) -> Result<(), Status> {
        self.request(|disk| async move { disk.lock_writable().await })
            .await?;
        self.locked.set(true);
        Ok(())
    }

    pub fn unlock_writable(&self) {
        self.locked.set(false);
        if self.state.get() == State::Up {
            let _ = self.disk.borrow().unlock_writable();
        }
    }
}
