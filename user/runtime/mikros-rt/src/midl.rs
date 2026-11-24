//! The system calls that generated MIDL bindings make, in the form their
//! runtime module expects.

use crate::sys::{self, Handle, Status};
use alloc::alloc::{Layout, alloc, dealloc};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

pub const SHOULD_WAIT: i32 = -22;
pub const PEER_CLOSED: i32 = -24;
const INVALID_ARGS: Status = -10;
const ACCESS_DENIED: Status = -13;
const BUFFER_TOO_SMALL: Status = -15;
const NO_MEMORY: Status = -4;

/// The largest message, in bytes and in handles.
const BYTES_MAX: usize = 65536;
const HANDLES_MAX: usize = 64;

const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
const SIGNAL_READABLE: u32 = 1 << 0;
const SIGNAL_PEER_CLOSED: u32 = 1 << 2;

fn status(s: Status) -> Result<(), i32> {
    if s == crate::OK {
        Ok(())
    } else {
        Err(s as i32)
    }
}

fn close_all(handles: &[u32]) {
    for &h in handles {
        // SAFETY: closing takes no pointers.
        unsafe { sys::handle_close(h) };
    }
}

pub fn handle_close(handle: u32) {
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(handle) };
}

/// The basic record of `handle`, or `None` when it cannot be inspected.
fn basic_info(handle: u32) -> Option<[u8; INFO_BASIC_SIZE]> {
    let mut record = [0u8; INFO_BASIC_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let s = unsafe {
        sys::object_get_info(
            handle,
            INFO_BASIC,
            record.as_mut_ptr(),
            INFO_BASIC_SIZE as u64,
            &mut len,
        )
    };
    (s == crate::OK && len >= INFO_BASIC_SIZE as u64).then_some(record)
}

/// The object type and rights of `handle`, or `None` when it cannot be
/// inspected.
pub fn handle_info(handle: u32) -> Option<(u32, u32)> {
    let record = basic_info(handle)?;
    let at = |i: usize| u32::from_le_bytes(record[i..i + 4].try_into().expect("4 bytes"));
    Some((at(16), at(20)))
}

/// The kernel object ID of the object of `handle`, or `None` when it
/// cannot be inspected.
pub fn koid(handle: u32) -> Option<u64> {
    let record = basic_info(handle)?;
    Some(u64::from_le_bytes(record[..8].try_into().expect("8 bytes")))
}

pub fn handle_replace(handle: u32, rights: u32) -> Result<u32, i32> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    status(unsafe { sys::handle_replace(handle, rights, &mut out) })?;
    Ok(out)
}

pub fn channel_create() -> Result<(u32, u32), i32> {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    status(unsafe { sys::channel_create(0, &mut a, &mut b) })?;
    Ok((a, b))
}

/// Writes a message, consuming `handles` even when it fails.
pub fn channel_write(channel: u32, bytes: &[u8], handles: &[u32]) -> Result<(), i32> {
    // SAFETY: the pointers and lengths describe the slices.
    let s = unsafe {
        sys::channel_write(
            channel,
            bytes.as_ptr(),
            bytes.len() as u64,
            handles.as_ptr(),
            handles.len() as u64,
        )
    };
    if s != crate::OK {
        // A failed write leaves the handles with the caller.
        close_all(handles);
    }
    status(s)
}

/// Reads the oldest message. It is read at once into the kept buffers of
/// the largest message, which `channel_call` uses too, and copied out to
/// buffers of its size; asking the kernel for the size first costs a
/// system call more for each message. When every slot is in use, the
/// size is asked for first.
pub fn channel_read(channel: u32) -> Result<(Vec<u8>, Vec<u32>), i32> {
    // Only a kept slot is worth it: buffers made for the read alone would
    // be mapped and unmapped for each message.
    let Some(lease) = lease_slot() else {
        return channel_read_sized(channel);
    };
    let reply = lease.buffers;
    // SAFETY: the lease owns the buffers, which are big enough.
    let (reply_bytes, reply_handles) = unsafe {
        (
            core::ptr::addr_of_mut!((*reply).bytes).cast::<u8>(),
            core::ptr::addr_of_mut!((*reply).handles).cast::<Handle>(),
        )
    };
    let (mut len, mut count) = (0u64, 0u64);
    // SAFETY: the pointers and capacities describe the buffers.
    let s = unsafe {
        sys::channel_read(
            channel,
            reply_bytes,
            BYTES_MAX as u64,
            reply_handles,
            HANDLES_MAX as u64,
            &mut len,
            &mut count,
        )
    };
    let message = if s == 0 {
        // SAFETY: the call stored `len` bytes and `count` handles.
        Some(unsafe {
            (
                core::slice::from_raw_parts(reply_bytes, len as usize).to_vec(),
                core::slice::from_raw_parts(reply_handles, count as usize).to_vec(),
            )
        })
    } else {
        None
    };
    if s == 0 {
        release(lease);
    } else {
        // Nothing was read, which a poll of an empty queue does often: the
        // slot is only given back, without the bookkeeping of a use.
        release_unused(lease);
    }
    match message {
        Some(message) => Ok(message),
        // A message beyond the largest size cannot be queued, but one that
        // is gets buffers of its own size.
        None if s == BUFFER_TOO_SMALL => channel_read_sized(channel),
        None => Err(s as i32),
    }
}

/// Reads the oldest message into buffers of its size, which the kernel
/// reports when it does not fit the buffers given, so that messages can be
/// read when the kept buffers cannot be had.
fn channel_read_sized(channel: u32) -> Result<(Vec<u8>, Vec<u32>), i32> {
    let (mut len, mut count) = (0u64, 0u64);
    loop {
        let mut bytes = vec![0u8; len as usize];
        let mut handles: Vec<Handle> = vec![0; count as usize];
        // SAFETY: the pointers and capacities describe the buffers.
        let s = unsafe {
            sys::channel_read(
                channel,
                bytes.as_mut_ptr(),
                bytes.len() as u64,
                handles.as_mut_ptr(),
                handles.len() as u64,
                &mut len,
                &mut count,
            )
        };
        // The call stored the sizes of a message too large for the
        // buffers. Another thread may take it first, so this repeats.
        if s == BUFFER_TOO_SMALL {
            continue;
        }
        status(s)?;
        bytes.truncate(len as usize);
        handles.truncate(count as usize);
        return Ok((bytes, handles));
    }
}

/// The buffers that a call's reply goes to: the largest message and
/// the most handles.
struct Reply {
    bytes: [u8; BYTES_MAX],
    handles: [Handle; HANDLES_MAX],
}

/// How many calls at once get kept buffers, one per thread that is calling
/// at the same time.
const SLOTS: usize = 8;
/// How long, in processor timestamp counter ticks, a buffer that is not
/// the first one stays unused before it is given back: about a second.
const IDLE_TICKS: u64 = 3_000_000_000;

/// A slot of kept reply buffers: whoever sets `busy` owns them.
struct Slot {
    busy: AtomicBool,
    buffers: AtomicPtr<Reply>,
    /// When the slot was last used, by `ticks`.
    idle_since: AtomicU64,
}

/// The first slot's buffers, which are zero pages of the program's image
/// that nothing maps until a call uses them, so that calls made before
/// the allocator can map memory have them too.
struct First(UnsafeCell<MaybeUninit<Reply>>);

// SAFETY: only the owner of the first slot uses it.
unsafe impl Sync for First {}

static FIRST: First = First(UnsafeCell::new(MaybeUninit::uninit()));

#[allow(clippy::declare_interior_mutable_const)]
const UNUSED: Slot = Slot {
    busy: AtomicBool::new(false),
    buffers: AtomicPtr::new(core::ptr::null_mut()),
    idle_since: AtomicU64::new(0),
};

static SLOTS_KEPT: [Slot; SLOTS] = {
    let mut slots = [UNUSED; SLOTS];
    slots[0] = Slot {
        busy: AtomicBool::new(false),
        buffers: AtomicPtr::new(FIRST.0.get().cast()),
        idle_since: AtomicU64::new(0),
    };
    slots
};

/// A time that only grows, in ticks that last about a third of a
/// nanosecond.
fn ticks() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: reading the timestamp counter has no preconditions.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let mut now = 0;
        // SAFETY: the pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        now as u64 * 3
    }
}

fn reply_layout() -> Layout {
    Layout::new::<Reply>()
}

/// The buffers that a call has: a slot's, or its own when the slots are
/// all in use.
struct Lease {
    slot: Option<usize>,
    buffers: *mut Reply,
}

/// Takes the buffers of a free slot, made when the slot has none, or
/// `None` when every slot is in use or the memory is not there.
fn lease_slot() -> Option<Lease> {
    for (at, slot) in SLOTS_KEPT.iter().enumerate() {
        if slot
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            continue;
        }
        let mut buffers = slot.buffers.load(Ordering::Relaxed);
        if buffers.is_null() {
            // SAFETY: the layout is not empty.
            buffers = unsafe { alloc(reply_layout()) }.cast::<Reply>();
            if buffers.is_null() {
                slot.busy.store(false, Ordering::Release);
                continue;
            }
            slot.buffers.store(buffers, Ordering::Relaxed);
        }
        return Some(Lease {
            slot: Some(at),
            buffers,
        });
    }
    None
}

/// Takes the buffers of a free slot, or buffers of the call's own when the
/// slots are all in use: none when the memory is not there.
fn lease() -> Option<Lease> {
    lease_slot().or_else(|| {
        // SAFETY: the layout is not empty.
        let buffers: *mut Reply = unsafe { alloc(reply_layout()) }.cast();
        (!buffers.is_null()).then_some(Lease {
            slot: None,
            buffers,
        })
    })
}

/// Gives the buffers of a lease that nothing used back, without counting
/// it as a use.
fn release_unused(lease: Lease) {
    match lease.slot {
        Some(at) => SLOTS_KEPT[at].busy.store(false, Ordering::Release),
        // SAFETY: the buffers came from `alloc` with this layout.
        None => unsafe { dealloc(lease.buffers.cast(), reply_layout()) },
    }
}

/// Gives the buffers back. A slot keeps its buffers for the next call,
/// and the slots after the first give theirs up once they have been
/// unused for `IDLE_TICKS`, so that threads that called once do not hold
/// memory for good.
fn release(lease: Lease) {
    let Some(at) = lease.slot else {
        // SAFETY: the buffers came from `alloc` with this layout.
        unsafe { dealloc(lease.buffers.cast(), reply_layout()) };
        return;
    };
    let now = ticks();
    SLOTS_KEPT[at].idle_since.store(now, Ordering::Relaxed);
    SLOTS_KEPT[at].busy.store(false, Ordering::Release);
    for slot in &SLOTS_KEPT[1..] {
        if slot.buffers.load(Ordering::Relaxed).is_null()
            || now.wrapping_sub(slot.idle_since.load(Ordering::Relaxed)) < IDLE_TICKS
            || slot
                .busy
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            continue;
        }
        // Still unused for as long, now that the slot is ours.
        let buffers = slot.buffers.load(Ordering::Relaxed);
        if !buffers.is_null()
            && now.wrapping_sub(slot.idle_since.load(Ordering::Relaxed)) >= IDLE_TICKS
        {
            slot.buffers.store(core::ptr::null_mut(), Ordering::Relaxed);
            // SAFETY: the buffers came from `alloc` with this layout and
            // nobody else has the slot.
            unsafe { dealloc(buffers.cast(), reply_layout()) };
        }
        slot.busy.store(false, Ordering::Release);
    }
}

/// Makes a call, consuming `handles` even when it fails, and returns the
/// reply. The kernel replaces the request's transaction ID.
///
/// The reply goes to buffers of the largest size that stay with the
/// caller's slot between calls, since mapping and faulting in 64 KiB for
/// each call cost more than the call.
pub fn channel_call(
    channel: u32,
    bytes: &[u8],
    handles: &[u32],
    deadline: i64,
) -> Result<(Vec<u8>, Vec<u32>), i32> {
    let Some(lease) = lease() else {
        close_all(handles);
        return Err(NO_MEMORY as i32);
    };
    let reply = lease.buffers;
    // SAFETY: the lease owns the buffers, which are big enough.
    let (reply_bytes, reply_handles) = unsafe {
        (
            core::ptr::addr_of_mut!((*reply).bytes).cast::<u8>(),
            core::ptr::addr_of_mut!((*reply).handles).cast::<Handle>(),
        )
    };
    let args: [u64; 8] = [
        bytes.as_ptr() as u64,
        bytes.len() as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
        reply_bytes as u64,
        BYTES_MAX as u64,
        reply_handles as u64,
        HANDLES_MAX as u64,
    ];
    let mut out = [0u64; 2];
    // SAFETY: the arguments and the buffers they point to are valid.
    let s = unsafe {
        sys::channel_call(
            channel,
            0,
            deadline,
            args.as_ptr().cast(),
            out.as_mut_ptr().cast(),
        )
    };
    let result = match s {
        crate::OK => {
            let (len, count) = (out[0] as usize, out[1] as usize);
            // SAFETY: the kernel wrote `len` bytes and `count` handles to
            // the buffers, which the lease still owns.
            let copied = unsafe {
                (
                    core::slice::from_raw_parts(reply_bytes, len).to_vec(),
                    core::slice::from_raw_parts(reply_handles, count).to_vec(),
                )
            };
            Ok(copied)
        }
        // Failures about the handles themselves leave them with the
        // caller, whose handles this call consumes.
        INVALID_ARGS | ACCESS_DENIED => {
            close_all(handles);
            Err(s as i32)
        }
        // On every other failure the kernel took them: it wrote them, or
        // closed them when it did not (MIKSPEC-0009.R36).
        _ => Err(s as i32),
    };
    release(lease);
    result
}

/// Waits until `channel` is readable or its peer closed.
pub fn wait_readable(channel: u32, deadline: i64) -> Result<(), i32> {
    let mut observed = 0;
    // SAFETY: the out pointer is valid for the call.
    status(unsafe {
        sys::object_wait_one(
            channel,
            SIGNAL_READABLE | SIGNAL_PEER_CLOSED,
            deadline,
            &mut observed,
        )
    })
}

/// The executor's reactor, which asynchronous bindings wait through.
pub type Reactor = crate::exec::Reactor;

/// Completes once `channel` is readable or its peer closed.
pub async fn readable(reactor: &Reactor, channel: u32) -> Result<(), i32> {
    reactor
        .readable(channel)
        .await
        .map(|_| ())
        .map_err(|s| s as i32)
}
