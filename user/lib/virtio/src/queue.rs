//! Split virtqueues: their descriptor table, available ring and used
//! ring in one contiguous memory object that the device reaches through
//! the driver's bus transaction initiator, and the buffers on them.

use crate::caps::Structures;
use crate::{Error, kernel};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::sync::atomic::{Ordering, fence};
use core::task::{Poll, Waker};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const PAGE_SIZE: u64 = 4096;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const DEVICE_READ: u32 = 1 << 0;
const DEVICE_WRITE: u32 = 1 << 1;
/// Descriptor flags: the chain goes on, and the device writes the
/// buffer. The used ring's flag that asks for no notifications.
const DESC_NEXT: u16 = 1 << 0;
const DESC_WRITE: u16 = 1 << 1;
const USED_NO_NOTIFY: u16 = 1 << 0;

/// One buffer of a request: its physical address and length, and
/// whether the device writes it, rather than reads it.
#[derive(Clone, Copy, Debug)]
pub struct Buffer {
    pub address: u64,
    pub len: u32,
    pub device_writes: bool,
}

/// What a queue keeps track of: the free descriptors, the next index of
/// the available ring and the last seen of the used ring, the requests
/// in flight by the head of their chain, and the tasks waiting for free
/// descriptors.
struct State {
    free: Vec<u16>,
    avail_idx: u16,
    used_idx: u16,
    in_flight: BTreeMap<u16, Flight>,
    waiting: Vec<Waker>,
}

/// A request in flight. Its descriptors stay taken until its task has
/// the length the device wrote, so that no other request reuses its head
/// meanwhile, or until the device returns it if its task went.
struct Flight {
    chain: Vec<u16>,
    /// The length the device wrote, once it returned the request.
    done: Option<u32>,
    waker: Option<Waker>,
    /// Whether its task went before the device returned it.
    abandoned: bool,
}

impl State {
    /// Gives `chain` back to the free descriptors, and wakes the tasks
    /// that wait for some.
    fn free_chain(&mut self, chain: &[u16], woken: &mut Vec<Waker>) {
        self.free.extend_from_slice(chain);
        woken.append(&mut self.waiting);
    }
}

/// Lets go of a request whose task goes, whether or not the device has
/// returned it.
struct InFlight<'a> {
    queue: &'a Queue,
    head: u16,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut state = self.queue.state.borrow_mut();
        let Some(flight) = state.in_flight.get_mut(&self.head) else {
            return;
        };
        if flight.done.is_none() {
            // The device still has it: its descriptors go back when it
            // returns it.
            flight.abandoned = true;
            return;
        }
        let flight = state.in_flight.remove(&self.head).expect("in flight");
        let mut woken = Vec::new();
        state.free_chain(&flight.chain, &mut woken);
        drop(state);
        for waker in woken {
            waker.wake();
        }
    }
}

/// A split virtqueue of `size` entries.
pub struct Queue {
    index: u16,
    size: u16,
    at: u64,
    avail: u64,
    used: u64,
    notify: Option<(Rc<Structures>, u64)>,
    state: RefCell<State>,
    _mem: OwnedHandle,
    _pmt: OwnedHandle,
}

/// Where the parts of a queue of `size` entries lie in its memory, and
/// the memory's size: the descriptor table at 0, the available ring
/// after it and the used ring after that, aligned to 4 bytes.
fn layout(size: u16) -> (u64, u64, u64) {
    let n = u64::from(size);
    let avail = 16 * n;
    let used = (avail + 6 + 2 * n).next_multiple_of(4);
    let total = (used + 6 + 8 * n).next_multiple_of(PAGE_SIZE);
    (avail, used, total)
}

impl Queue {
    /// A queue of `size` entries, in a contiguous memory object made from
    /// the `DMA` resource `dma`, mapped into `process` and pinned for the
    /// device through `bti`. Returns it with the physical addresses of
    /// its descriptor table, available ring and used ring.
    pub(crate) fn new(
        index: u16,
        size: u16,
        dma: &OwnedHandle,
        bti: &OwnedHandle,
        process: sys::Handle,
    ) -> Result<(Queue, [u64; 3]), Error> {
        let (avail, used, total) = layout(size);
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe { sys::mem_create_contiguous(dma.raw(), total, 12, 0, &mut mem) })?;
        // SAFETY: the kernel just created the handle for us.
        let mem = unsafe { OwnedHandle::from_raw(mem) };
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        kernel(unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                mem.raw(),
                0,
                total,
                &mut at,
            )
        })?;
        let mut pmt = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe {
            sys::bti_pin(
                bti.raw(),
                mem.raw(),
                0,
                total,
                DEVICE_READ | DEVICE_WRITE,
                &mut pmt,
            )
        })?;
        // SAFETY: the kernel just created the handle for us.
        let pmt = unsafe { OwnedHandle::from_raw(pmt) };
        let mut physical = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe { sys::mem_get_physical(mem.raw(), &mut physical) })?;
        let queue = Queue {
            index,
            size,
            at,
            avail: at + avail,
            used: at + used,
            notify: None,
            state: RefCell::new(State {
                free: (0..size).rev().collect(),
                avail_idx: 0,
                used_idx: 0,
                in_flight: BTreeMap::new(),
                waiting: Vec::new(),
            }),
            _mem: mem,
            _pmt: pmt,
        };
        Ok((queue, [physical, physical + avail, physical + used]))
    }

    /// Notifications for the queue go to `offset` in the notification
    /// structure of `structures`.
    pub(crate) fn set_notify(&mut self, structures: Rc<Structures>, offset: u64) {
        self.notify = Some((structures, offset));
    }

    pub fn index(&self) -> u16 {
        self.index
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    /// Puts the request of `buffers`, which the device reads and writes
    /// in that order, on the queue, notifies the device unless it asked
    /// for no notifications, and waits until the device returned it.
    /// Returns the number of bytes the device wrote. Waits for free
    /// descriptors when there are too few.
    pub async fn submit(&self, buffers: &[Buffer]) -> u32 {
        assert!(
            !buffers.is_empty() && buffers.len() <= usize::from(self.size),
            "a request needs from one descriptor to the queue's size"
        );
        let chain: Vec<u16> = poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if state.free.len() < buffers.len() {
                state.waiting.push(cx.waker().clone());
                return Poll::Pending;
            }
            let at = state.free.len() - buffers.len();
            Poll::Ready(state.free.split_off(at))
        })
        .await;
        for (i, (buffer, &desc)) in buffers.iter().zip(&chain).enumerate() {
            let next = chain.get(i + 1).copied();
            let mut flags = if buffer.device_writes { DESC_WRITE } else { 0 };
            if next.is_some() {
                flags |= DESC_NEXT;
            }
            let entry = self.at + 16 * u64::from(desc);
            // SAFETY: the descriptor lies in the mapped table.
            unsafe {
                core::ptr::write_volatile(entry as *mut u64, buffer.address);
                core::ptr::write_volatile((entry + 8) as *mut u32, buffer.len);
                core::ptr::write_volatile((entry + 12) as *mut u16, flags);
                core::ptr::write_volatile((entry + 14) as *mut u16, next.unwrap_or(0));
            }
        }
        let head = chain[0];
        {
            let mut state = self.state.borrow_mut();
            let flight = Flight {
                chain,
                done: None,
                waker: None,
                abandoned: false,
            };
            state.in_flight.insert(head, flight);
            let slot = u64::from(state.avail_idx % self.size);
            // SAFETY: the ring entry lies in the mapped available ring.
            unsafe { core::ptr::write_volatile((self.avail + 4 + 2 * slot) as *mut u16, head) };
            state.avail_idx = state.avail_idx.wrapping_add(1);
            fence(Ordering::SeqCst);
            // SAFETY: the index lies in the mapped available ring.
            unsafe { core::ptr::write_volatile((self.avail + 2) as *mut u16, state.avail_idx) };
            fence(Ordering::SeqCst);
        }
        // SAFETY: the flags lie in the mapped used ring.
        let used_flags = unsafe { core::ptr::read_volatile(self.used as *const u16) };
        if used_flags & USED_NO_NOTIFY == 0
            && let Some((structures, offset)) = &self.notify
        {
            structures.notify.write16(*offset, self.index);
        }
        // The guard frees the descriptors once the length is taken, or
        // when the task goes.
        let _guard = InFlight { queue: self, head };
        poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            let entry = state.in_flight.get_mut(&head).expect("in flight");
            match entry.done {
                Some(len) => Poll::Ready(len),
                None => {
                    entry.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }

    /// Takes the requests that the device returned on the used ring and
    /// wakes the tasks waiting for them, which free their descriptors
    /// once they have the length; those of a request whose task went are
    /// freed here.
    pub(crate) fn process_used(&self) {
        let mut state = self.state.borrow_mut();
        let mut woken = Vec::new();
        loop {
            fence(Ordering::SeqCst);
            // SAFETY: the index lies in the mapped used ring.
            let used_idx = unsafe { core::ptr::read_volatile((self.used + 2) as *const u16) };
            if used_idx == state.used_idx {
                break;
            }
            let slot = u64::from(state.used_idx % self.size);
            let entry = self.used + 4 + 8 * slot;
            // SAFETY: the entry lies in the mapped used ring.
            let (id, len) = unsafe {
                (
                    core::ptr::read_volatile(entry as *const u32) as u16,
                    core::ptr::read_volatile((entry + 4) as *const u32),
                )
            };
            state.used_idx = state.used_idx.wrapping_add(1);
            let Some(flight) = state.in_flight.get_mut(&id) else {
                continue;
            };
            if flight.abandoned {
                let flight = state.in_flight.remove(&id).expect("in flight");
                state.free_chain(&flight.chain, &mut woken);
                continue;
            }
            flight.done = Some(len);
            woken.extend(flight.waker.take());
        }
        drop(state);
        for waker in woken {
            waker.wake();
        }
    }
}
