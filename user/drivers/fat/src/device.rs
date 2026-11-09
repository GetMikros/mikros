//! The partition under a volume: its `Block` channel, through one buffer
//! mapped into the server, with sync calls.
//!
//! The channel outlives the partition driver's restarts
//! (MIKSPEC-0044.R19). When it closes before a request completes, the
//! request waits. `Reconnect` (MIKSPEC-0025.R34) reaches the driver's
//! thread, which creates the buffer again on the new channel and hands
//! the new link over through the `Gate`. The waiting request takes it,
//! copies the old buffer into the new one, so that a write that had not
//! completed writes the same data, and goes again. The server makes one
//! request at a time, so the requests keep their order. When no new
//! channel comes within 10 seconds of the old one closing, or the driver
//! stops while it is closed, the request fails, and so do later ones
//! until a new channel comes.

use alloc::boxed::Box;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use mikros_fat::{Device, Error};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockClient, BlockFlags, BlockInfo};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The size of the buffer for requests, the most one request moves.
const BUFFER: u64 = 256 << 10;
const FOREVER: i64 = i64::MAX;
/// How long a request waits for a new channel once the old one closed.
const RECONNECT_LIMIT: i64 = 10_000_000_000;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// A channel to the partition and the buffer on it, mapped into the
/// server.
struct Link {
    block: BlockClient,
    buffer: u32,
    at: u64,
    process: sys::Handle,
}

impl Link {
    /// A link over `block`, with a new buffer mapped into `process`. None
    /// when the disk gives no buffer.
    fn new(block: BlockClient, process: sys::Handle) -> Option<Link> {
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
        (mapped == mikros_rt::OK).then_some(Link {
            block,
            buffer: created.buffer,
            at,
            process,
        })
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        // SAFETY: the call takes no pointers; the range is the link's
        // mapping, which nothing uses once the link goes.
        unsafe { sys::vm_unmap(self.process, self.at, BUFFER) };
    }
}

/// What the driver's thread hands the server's thread: the link on the
/// newest channel that `Reconnect` gave, and whether the driver stops.
pub struct Gate {
    /// A link that the server's thread has not taken yet, or null.
    next: AtomicPtr<Link>,
    stopped: AtomicBool,
    /// Changes whenever the two above do; a waiting request waits on it.
    changed: AtomicU32,
}

impl Gate {
    pub const fn new() -> Gate {
        Gate {
            next: AtomicPtr::new(ptr::null_mut()),
            stopped: AtomicBool::new(false),
            changed: AtomicU32::new(0),
        }
    }

    fn changed(&self) {
        self.changed.fetch_add(1, Ordering::Release);
        let mut woken = 0;
        // SAFETY: the word stays valid while threads wait on it.
        unsafe { sys::futex_wake(self.changed.as_ptr(), u32::MAX, &mut woken) };
    }

    /// Continues on `bus`, the channel that `Reconnect` gave: creates the
    /// buffer on it, for the next request to take. `NO_RESOURCES` when
    /// the partition gives no buffer.
    pub fn reconnect(&self, bus: OwnedHandle, process: sys::Handle) -> Result<(), Status> {
        let block = BlockClient::new(ClientEnd::new(bus));
        let link = Link::new(block, process).ok_or(Status::NoResources)?;
        let older = self
            .next
            .swap(Box::into_raw(Box::new(link)), Ordering::AcqRel);
        if !older.is_null() {
            // SAFETY: a link handed over and never taken, which nothing
            // else holds once it is swapped out.
            drop(unsafe { Box::from_raw(older) });
        }
        self.changed();
        Ok(())
    }

    /// Fails the request that waits for a new channel, and the later ones
    /// while there is none, as the driver stops.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.changed();
    }

    /// The link that `reconnect` handed over, if it has not been taken.
    fn take(&self) -> Option<Box<Link>> {
        let next = self.next.swap(ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: `reconnect` made the pointer from a box, and the swap
        // gave it to this caller alone.
        (!next.is_null()).then(|| unsafe { Box::from_raw(next) })
    }
}

pub struct BlockDevice {
    link: Box<Link>,
    gate: &'static Gate,
    block_size: u64,
    /// The most bytes one request moves.
    max: u64,
    pub info: BlockInfo,
    /// When the channel was found closed, while no new one has come.
    lost_at: Option<i64>,
    /// Whether requests fail at once: no new channel came in time, or
    /// the driver stopped while there was none.
    failed: bool,
}

impl BlockDevice {
    /// The device behind `block`, with a buffer mapped into `process`,
    /// which takes the new channels that reach `gate`. None when the
    /// disk cannot give a buffer.
    pub fn new(
        block: BlockClient,
        process: sys::Handle,
        gate: &'static Gate,
    ) -> Option<BlockDevice> {
        let info = block.get_info(FOREVER).ok()?.info;
        let block_size = u64::from(info.block_size);
        if block_size == 0 || !BUFFER.is_multiple_of(block_size) {
            return None;
        }
        let link = Link::new(block, process)?;
        let max = (u64::from(info.max_transfer_blocks.max(1)) * block_size).min(BUFFER);
        Some(BlockDevice {
            link: Box::new(link),
            gate,
            block_size,
            max,
            info,
            lost_at: None,
            failed: false,
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
        // SAFETY: the link's buffer is mapped at `at` for `BUFFER` bytes,
        // and only this device uses it.
        unsafe { core::slice::from_raw_parts_mut(self.link.at as *mut u8, BUFFER as usize) }
    }

    /// Continues on the link that the gate holds, if there is one, with
    /// what the old buffer holds.
    fn switch(&mut self) {
        let Some(next) = self.gate.take() else {
            return;
        };
        // SAFETY: both buffers are mapped for `BUFFER` bytes, and only
        // this device uses them.
        unsafe {
            ptr::copy_nonoverlapping(
                self.link.at as *const u8,
                next.at as *mut u8,
                BUFFER as usize,
            )
        };
        self.link = next;
        self.lost_at = None;
        self.failed = false;
    }

    /// Makes `call` on the channel of the moment, and again on a new one
    /// when the channel closes before the call completes. `call` gives
    /// whether the partition did what it asked, or `Err` when the channel
    /// failed.
    fn request(&mut self, call: impl Fn(&Link) -> Result<bool, ()>) -> Result<(), Error> {
        loop {
            self.switch();
            if self.failed {
                return Err(Error::Io);
            }
            let Some(lost_at) = self.lost_at else {
                match call(&self.link) {
                    Ok(true) => return Ok(()),
                    Ok(false) => return Err(Error::Io),
                    Err(()) => self.lost_at = Some(now()),
                }
                continue;
            };
            let seen = self.gate.changed.load(Ordering::Acquire);
            if !self.gate.next.load(Ordering::Acquire).is_null() {
                continue;
            }
            let deadline = lost_at.saturating_add(RECONNECT_LIMIT);
            if self.gate.stopped.load(Ordering::Acquire) || now() >= deadline {
                self.failed = true;
                return Err(Error::Io);
            }
            // SAFETY: the word stays valid while the thread waits on it.
            unsafe { sys::futex_wait(self.gate.changed.as_ptr(), seen, deadline) };
        }
    }

    /// Reads `len` bytes, whole blocks, at `at` into the buffer.
    fn read_blocks(&mut self, at: u64, len: u64) -> Result<(), Error> {
        let (block, count) = (at / self.block_size, (len / self.block_size) as u32);
        self.request(|link| {
            let read = link.block.read(link.buffer, 0, block, count, FOREVER);
            read.map(|r| r.is_ok()).map_err(|_| ())
        })
    }

    fn write_blocks(&mut self, at: u64, len: u64) -> Result<(), Error> {
        let (block, count) = (at / self.block_size, (len / self.block_size) as u32);
        self.request(|link| {
            let written = link
                .block
                .write(link.buffer, 0, block, count, false, FOREVER);
            written.map(|r| r.is_ok()).map_err(|_| ())
        })
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
        self.request(|link| link.block.flush(FOREVER).map(|r| r.is_ok()).map_err(|_| ()))
    }
}
