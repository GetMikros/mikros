// SPDX-License-Identifier: GPL-2.0
//! The partition under the file system, which the shim reads and writes
//! through the host layer's disk calls: its `Block` channel, through one
//! buffer mapped into the server and cut into slots, one for each
//! request in flight, so that the shim's workers' requests can run at
//! once.
//!
//! The channel outlives the partition driver's restarts
//! (MIKSPEC-0044.R19): requests are numbered, and when the channel
//! closes before one completes, its thread waits. `Reconnect`
//! (MIKSPEC-0025.R34) gives a new channel, on which the disk creates its
//! buffer again; then the requests that had not completed go again, one
//! at a time and in the order in which they first came, before any
//! other, each copying what it writes into the new buffer. When no new
//! channel is up within 10 seconds, or the driver stops, they fail.

use crate::host::Spin;
use alloc::collections::{BTreeSet, VecDeque};
use alloc::sync::Arc;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::storage::{BlockClient, BlockFlags, BlockInfo};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const FOREVER: i64 = i64::MAX;
/// The requests in flight at most, and the most bytes each moves.
const SLOTS: usize = 16;
const SLOT_MAX: u64 = 1 << 20;
/// How long requests wait for a new channel.
const RECONNECT_LIMIT: i64 = 10_000_000_000;
const EIO: c_int = 5;
const EINVAL: c_int = 22;

/// A channel to the partition and its buffer, mapped into the server.
struct Link {
    block: BlockClient,
    buffer: u32,
    at: u64,
    process: sys::Handle,
    size: u64,
}

// SAFETY: the client's only state is its channel's handle, which it
// changes only when it closes the channel after an invalid reply; the
// kernel takes calls on one channel from any number of threads.
unsafe impl Send for Link {}
unsafe impl Sync for Link {}

impl Link {
    /// A link over `block`, with a new buffer of `size` bytes mapped into
    /// `process`. None when the disk gives no buffer.
    fn new(block: BlockClient, size: u64, process: sys::Handle) -> Option<Link> {
        let created = block.create_buffer(size, FOREVER).ok()?.ok()?;
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let mapped = unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                created.memory.raw(),
                0,
                size,
                &mut at,
            )
        };
        (mapped == mikros_rt::OK).then_some(Link {
            block,
            buffer: created.buffer,
            at,
            process,
            size,
        })
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        // SAFETY: the call takes no pointers; the range is the link's
        // mapping, which nothing uses once the link goes.
        unsafe { sys::vm_unmap(self.process, self.at, self.size) };
    }
}

/// Which requests may go: whether the channel is up, or has failed for
/// good, which channel is current, and the numbered requests.
struct Gate {
    up: bool,
    failed: bool,
    generation: u64,
    /// When the channel went down.
    lost_at: i64,
    next: u64,
    unfinished: BTreeSet<u64>,
    replay: VecDeque<u64>,
}

/// What an attempt at a request came to.
enum Attempt {
    Done(bool),
    /// The channel failed.
    Lost,
}

pub struct Disk {
    link: Spin<Arc<Link>>,
    slot_size: u64,
    pub info: BlockInfo,
    /// A bit for each slot in use, and a count that changes when one
    /// is given back, which takers wait on.
    used: AtomicU32,
    freed: AtomicU32,
    gate: Spin<Gate>,
    /// Changes whenever the gate does, which waiting threads wait on.
    changed: AtomicU32,
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

impl Disk {
    /// The disk behind `block`, with its buffer mapped into `process`.
    /// None when the disk cannot give one.
    pub fn new(block: BlockClient, process: sys::Handle) -> Option<Disk> {
        let info = block.get_info(FOREVER).ok()?.info;
        let block_size = u64::from(info.block_size);
        if block_size == 0 || !block_size.is_power_of_two() || block_size > 4096 {
            return None;
        }
        let max = u64::from(info.max_transfer_blocks.max(1)) * block_size;
        let slot_size = max.min(SLOT_MAX) / block_size * block_size;
        let link = Link::new(block, slot_size * SLOTS as u64, process)?;
        Some(Disk {
            link: Spin::new(Arc::new(link)),
            slot_size,
            info,
            used: AtomicU32::new(0),
            freed: AtomicU32::new(0),
            gate: Spin::new(Gate {
                up: true,
                failed: false,
                generation: 0,
                lost_at: 0,
                next: 0,
                unfinished: BTreeSet::new(),
                replay: VecDeque::new(),
            }),
            changed: AtomicU32::new(0),
        })
    }

    pub fn size(&self) -> u64 {
        self.info.block_count * u64::from(self.info.block_size)
    }

    pub fn read_only(&self) -> bool {
        self.info.flags.0 & BlockFlags::READ_ONLY.0 != 0
    }

    /// Whether the disk takes `Discard`.
    pub fn discards(&self) -> bool {
        self.info.flags.0 & BlockFlags::DISCARD.0 != 0
    }

    /// Wakes the threads that wait for the gate to change.
    fn changed(&self) {
        self.changed.fetch_add(1, Ordering::Release);
        let mut woken = 0;
        // SAFETY: the word stays valid while threads wait on it.
        unsafe { sys::futex_wake(self.changed.as_ptr(), u32::MAX, &mut woken) };
    }

    /// Makes request `attempt`, on the channel of the moment, again on a
    /// new channel when the old one fails before it completes: whether
    /// it succeeded.
    fn request(&self, attempt: impl Fn(&Link) -> Attempt) -> bool {
        let number = {
            let mut gate = self.gate.lock();
            gate.next += 1;
            gate.next
        };
        loop {
            let Some((link, generation)) = self.turn(number) else {
                self.finish(number);
                return false;
            };
            match attempt(&link) {
                Attempt::Done(ok) => {
                    self.finish(number);
                    return ok;
                }
                Attempt::Lost => self.lost(generation),
            }
        }
    }

    /// Waits until request `number` may go: not while the channel is
    /// down, nor while requests go again, unless it is the next of those.
    /// The channel and its generation, or None once the disk failed.
    fn turn(&self, number: u64) -> Option<(Arc<Link>, u64)> {
        loop {
            let seen = self.changed.load(Ordering::Acquire);
            let deadline = {
                let mut gate = self.gate.lock();
                if gate.failed {
                    return None;
                }
                if gate.up {
                    match gate.replay.front() {
                        Some(&first) if first != number => FOREVER,
                        _ => {
                            gate.unfinished.insert(number);
                            let generation = gate.generation;
                            drop(gate);
                            return Some((self.link.lock().clone(), generation));
                        }
                    }
                } else if now() >= gate.lost_at + RECONNECT_LIMIT {
                    gate.failed = true;
                    drop(gate);
                    self.changed();
                    return None;
                } else {
                    gate.lost_at + RECONNECT_LIMIT
                }
            };
            // SAFETY: the word stays valid while the thread waits on it.
            unsafe { sys::futex_wait(self.changed.as_ptr(), seen, deadline) };
        }
    }

    /// Ends request `number`, which lets the next to go again go. One
    /// that completed on the old channel after a new one came is not
    /// sent again.
    fn finish(&self, number: u64) {
        let mut gate = self.gate.lock();
        gate.unfinished.remove(&number);
        gate.replay.retain(|&n| n != number);
        drop(gate);
        self.changed();
    }

    /// Takes the disk down when the channel of `generation` failed.
    fn lost(&self, generation: u64) {
        let mut gate = self.gate.lock();
        if gate.generation == generation && gate.up {
            gate.up = false;
            gate.lost_at = now();
        }
        drop(gate);
        self.changed();
    }

    /// Continues on `bus`, the channel that `Reconnect` gave: creates the
    /// buffer again, and lets the requests that had not completed go
    /// again, in order. `NO_RESOURCES`, staying down, when the partition
    /// gives no buffer.
    pub fn reconnect(&self, bus: OwnedHandle, process: sys::Handle) -> Result<(), Status> {
        {
            let mut gate = self.gate.lock();
            if gate.up {
                gate.lost_at = now();
            }
            gate.up = false;
            gate.generation += 1;
        }
        self.changed();
        let block = BlockClient::new(ClientEnd::new(bus));
        let link =
            Link::new(block, self.slot_size * SLOTS as u64, process).ok_or(Status::NoResources)?;
        *self.link.lock() = Arc::new(link);
        {
            let mut gate = self.gate.lock();
            gate.replay = gate.unfinished.iter().copied().collect();
            gate.up = true;
            gate.failed = false;
        }
        self.changed();
        Ok(())
    }

    /// Fails the requests that wait for a new channel, and all later
    /// ones, as when the driver stops while the channel is down.
    pub fn fail(&self) {
        let mut gate = self.gate.lock();
        if gate.up {
            return;
        }
        gate.failed = true;
        drop(gate);
        self.changed();
    }

    /// A free slot, once there is one.
    fn take(&self) -> usize {
        loop {
            let freed = self.freed.load(Ordering::Acquire);
            let used = self.used.load(Ordering::Acquire);
            let slot = (!used).trailing_zeros() as usize;
            if slot < SLOTS {
                let bit = 1 << slot;
                if self.used.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
                    return slot;
                }
                continue;
            }
            // SAFETY: the word stays valid while the thread waits on it.
            unsafe { sys::futex_wait(self.freed.as_ptr(), freed, FOREVER) };
        }
    }

    fn give_back(&self, slot: usize) {
        self.used.fetch_and(!(1 << slot), Ordering::Release);
        self.freed.fetch_add(1, Ordering::Release);
        let mut woken = 0;
        // SAFETY: as for waiting.
        unsafe { sys::futex_wake(self.freed.as_ptr(), 1, &mut woken) };
    }

    /// Reads or writes `len` bytes at `offset`, whole blocks, with `buf`,
    /// a slot's worth at a time: 0, or a negative errno.
    fn transfer(&self, write: bool, mut offset: u64, buf: *mut u8, len: usize) -> c_int {
        let block_size = u64::from(self.info.block_size);
        if !offset.is_multiple_of(block_size) || !(len as u64).is_multiple_of(block_size) {
            return -EINVAL;
        }
        let slot = self.take();
        let mut done = 0;
        let mut ret = 0;
        while done < len {
            let n = (len - done).min(self.slot_size as usize);
            let (block, count) = (offset / block_size, (n as u64 / block_size) as u32);
            let at = slot as u64 * self.slot_size;
            let ok = self.request(|link| {
                let slot_at = (link.at + at) as *mut u8;
                // SAFETY: the shim's buffer holds `len` bytes, which
                // nothing else touches during the call, and the slot
                // `slot_size` bytes, which only the thread that took it
                // touches until it gives it back.
                let result = if write {
                    unsafe { core::ptr::copy_nonoverlapping(buf.add(done), slot_at, n) };
                    link.block
                        .write(link.buffer, at, block, count, false, FOREVER)
                        .map(|r| r.is_ok())
                } else {
                    let read = link.block.read(link.buffer, at, block, count, FOREVER);
                    if let Ok(Ok(_)) = read {
                        unsafe { core::ptr::copy_nonoverlapping(slot_at, buf.add(done), n) };
                    }
                    read.map(|r| r.is_ok())
                };
                match result {
                    Ok(ok) => Attempt::Done(ok),
                    Err(_) => Attempt::Lost,
                }
            });
            if !ok {
                ret = -EIO;
                break;
            }
            done += n;
            offset += n as u64;
        }
        self.give_back(slot);
        ret
    }

    pub fn flush(&self) -> c_int {
        let ok = self.request(|link| match link.block.flush(FOREVER) {
            Ok(flushed) => Attempt::Done(flushed.is_ok()),
            Err(_) => Attempt::Lost,
        });
        if ok { 0 } else { -EIO }
    }

    /// Discards `len` bytes at `offset`, whole blocks: 0, or a negative
    /// errno.
    fn discard(&self, offset: u64, len: u64) -> c_int {
        let block_size = u64::from(self.info.block_size);
        if !offset.is_multiple_of(block_size) || !len.is_multiple_of(block_size) {
            return -EINVAL;
        }
        let (block, count) = (offset / block_size, len / block_size);
        let ok = self.request(|link| match link.block.discard(block, count, FOREVER) {
            Ok(discarded) => Attempt::Done(discarded.is_ok()),
            Err(_) => Attempt::Lost,
        });
        if ok { 0 } else { -EIO }
    }
}

/// The disk of a handle that `main` gave the shim.
fn disk<'a>(handle: *mut c_void) -> &'a Disk {
    // SAFETY: the shim passes back the handle, which points to a disk
    // that stays for the rest of the program.
    unsafe { &*handle.cast::<Disk>() }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_read(
    handle: *mut c_void,
    offset: u64,
    buf: *mut u8,
    len: usize,
) -> c_int {
    disk(handle).transfer(false, offset, buf, len)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_write(
    handle: *mut c_void,
    offset: u64,
    buf: *const u8,
    len: usize,
) -> c_int {
    disk(handle).transfer(true, offset, buf.cast_mut(), len)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_flush(handle: *mut c_void) -> c_int {
    disk(handle).flush()
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_discard(handle: *mut c_void, offset: u64, len: u64) -> c_int {
    disk(handle).discard(offset, len)
}
