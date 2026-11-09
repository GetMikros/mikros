//! The waits that futures register on the executor's port, and the
//! futures that complete when their packets arrive.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::sys::{self, Handle, Status};

pub const PACKET_SIZE: usize = 48;
const INTERRUPTED: Status = -27;
const TIMED_OUT: Status = -21;
/// The signal a timer sets when its deadline passes.
const SIGNALED: u32 = 1 << 3;

/// A packet taken from the port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub key: u64,
    pub kind: u32,
    pub status: i32,
    pub payload: [u8; 32],
}

impl Packet {
    fn from_bytes(b: &[u8; PACKET_SIZE]) -> Packet {
        let mut payload = [0; 32];
        payload.copy_from_slice(&b[16..]);
        Packet {
            key: u64::from_le_bytes(b[..8].try_into().expect("8 bytes")),
            kind: u32::from_le_bytes(b[8..12].try_into().expect("4 bytes")),
            status: i32::from_le_bytes(b[12..16].try_into().expect("4 bytes")),
            payload,
        }
    }

    fn word(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.payload[at..at + 4].try_into().expect("4 bytes"))
    }
}

/// What waiting on the port gave.
pub(super) enum Taken {
    Packet(Packet),
    /// A packet that only ends the wait, or an interrupted wait.
    Wakeup,
    /// No packet before the deadline.
    Empty,
}

/// The packets that arrived for one key, and the task to wake for them.
#[derive(Default)]
struct Slot {
    packets: VecDeque<Packet>,
    waker: Option<Waker>,
}

struct Inner {
    port: Handle,
    next_key: Cell<u64>,
    slots: RefCell<BTreeMap<u64, Slot>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: closing takes no pointers.
        unsafe { sys::handle_close(self.port) };
    }
}

/// The port of an executor and the waits registered on it. It belongs
/// to the executor's thread.
#[derive(Clone)]
pub struct Reactor(Rc<Inner>);

impl Reactor {
    pub(super) fn new() -> Result<Reactor, Status> {
        let mut port = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::port_create(0, &mut port) };
        if status != crate::OK {
            return Err(status);
        }
        Ok(Reactor(Rc::new(Inner {
            port,
            next_key: Cell::new(1),
            slots: RefCell::new(BTreeMap::new()),
        })))
    }

    /// The port, for calls that queue packets on it with a key from
    /// `packets`.
    pub fn port(&self) -> Handle {
        self.0.port
    }

    /// A new key, never 0, with a slot for its packets.
    fn key(&self) -> u64 {
        let key = self.0.next_key.get();
        self.0.next_key.set(key + 1);
        self.0.slots.borrow_mut().insert(key, Slot::default());
        key
    }

    fn release(&self, key: u64) {
        self.0.slots.borrow_mut().remove(&key);
    }

    /// The next packet for `key`, or `Pending` with `waker` kept.
    fn poll_packet(&self, key: u64, waker: &Waker) -> Poll<Packet> {
        let mut slots = self.0.slots.borrow_mut();
        let Some(slot) = slots.get_mut(&key) else {
            return Poll::Pending;
        };
        match slot.packets.pop_front() {
            Some(packet) => Poll::Ready(packet),
            None => {
                slot.waker = Some(waker.clone());
                Poll::Pending
            }
        }
    }

    /// Takes the next packet, waiting for one until `deadline`.
    pub(super) fn take(&self, deadline: i64) -> Taken {
        let mut bytes = [0u8; PACKET_SIZE];
        // SAFETY: the pointer refers to a whole packet.
        let status = unsafe { sys::port_wait(self.0.port, deadline, bytes.as_mut_ptr()) };
        match status {
            crate::OK => match Packet::from_bytes(&bytes) {
                packet if packet.key != 0 => Taken::Packet(packet),
                _ => Taken::Wakeup,
            },
            INTERRUPTED => Taken::Wakeup,
            TIMED_OUT => Taken::Empty,
            _ => panic!("waiting on the executor's port failed with {status}"),
        }
    }

    /// Hands `packet` to the wait of its key and wakes its task.
    pub(super) fn deliver(&self, packet: Packet) {
        let waker = {
            let mut slots = self.0.slots.borrow_mut();
            // A packet for a wait that was dropped is discarded.
            let Some(slot) = slots.get_mut(&packet.key) else {
                return;
            };
            slot.packets.push_back(packet);
            slot.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Completes with the signals of `handle` once one of `signals` is
    /// set.
    pub fn signals(&self, handle: Handle, signals: u32) -> Signals {
        Signals {
            reactor: self.clone(),
            handle,
            signals,
            key: None,
        }
    }

    /// Completes once `channel` has a message or its peer closed.
    pub fn readable(&self, channel: Handle) -> Signals {
        const READABLE: u32 = 1 << 0;
        const PEER_CLOSED: u32 = 1 << 2;
        self.signals(channel, READABLE | PEER_CLOSED)
    }

    /// Completes with the value of `timeline` once it reaches `value`.
    pub fn timeline(&self, timeline: Handle, value: u64) -> Timeline {
        Timeline {
            reactor: self.clone(),
            timeline,
            value,
            key: None,
        }
    }

    /// Completes once the monotonic clock reaches `deadline`.
    pub async fn sleep_until(&self, deadline: i64) -> Result<(), Status> {
        let mut timer = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::timer_create(0, &mut timer) };
        if status != crate::OK {
            return Err(status);
        }
        struct Close(Handle);
        impl Drop for Close {
            fn drop(&mut self) {
                // SAFETY: closing takes no pointers.
                unsafe { sys::handle_close(self.0) };
            }
        }
        let timer = Close(timer);
        // SAFETY: the call takes no pointers.
        let status = unsafe { sys::timer_set(timer.0, deadline) };
        if status != crate::OK {
            return Err(status);
        }
        self.signals(timer.0, SIGNALED).await.map(|_| ())
    }

    /// A stream of the packets queued with a key of its own, such as the
    /// page requests of a pager.
    pub fn packets(&self) -> Packets {
        Packets {
            key: self.key(),
            reactor: self.clone(),
        }
    }
}

/// The signals of an object, once one of those waited for is set.
pub struct Signals {
    reactor: Reactor,
    handle: Handle,
    signals: u32,
    key: Option<u64>,
}

impl Future for Signals {
    type Output = Result<u32, Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let key = match self.key {
            Some(key) => key,
            None => {
                let key = self.reactor.key();
                // SAFETY: the call takes no pointers.
                let status = unsafe {
                    sys::object_wait_async(self.handle, self.reactor.port(), key, self.signals, 0)
                };
                if status != crate::OK {
                    self.reactor.release(key);
                    return Poll::Ready(Err(status));
                }
                self.key = Some(key);
                key
            }
        };
        self.reactor.poll_packet(key, cx.waker()).map(|packet| {
            self.reactor.release(key);
            self.key = None;
            Ok(packet.word(4))
        })
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            // The wait may have ended already; then there is nothing to
            // cancel.
            // SAFETY: the call takes no pointers.
            unsafe { sys::port_cancel(self.reactor.port(), self.handle, key) };
            self.reactor.release(key);
        }
    }
}

/// The value of a timeline, once it reaches the value waited for.
pub struct Timeline {
    reactor: Reactor,
    timeline: Handle,
    value: u64,
    key: Option<u64>,
}

impl Future for Timeline {
    type Output = Result<u64, Status>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let key = match self.key {
            Some(key) => key,
            None => {
                let key = self.reactor.key();
                // SAFETY: the call takes no pointers.
                let status = unsafe {
                    sys::timeline_wait_async(self.timeline, self.reactor.port(), key, self.value)
                };
                if status != crate::OK {
                    self.reactor.release(key);
                    return Poll::Ready(Err(status));
                }
                self.key = Some(key);
                key
            }
        };
        self.reactor.poll_packet(key, cx.waker()).map(|packet| {
            self.reactor.release(key);
            self.key = None;
            Ok(u64::from_le_bytes(
                packet.payload[..8].try_into().expect("8 bytes"),
            ))
        })
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            // SAFETY: the call takes no pointers.
            unsafe { sys::port_cancel(self.reactor.port(), self.timeline, key) };
            self.reactor.release(key);
        }
    }
}

/// The packets queued on the executor's port with `key`.
pub struct Packets {
    reactor: Reactor,
    key: u64,
}

impl Packets {
    /// The key to queue packets with, on `Reactor::port`.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// The next packet.
    pub fn next(&self) -> NextPacket<'_> {
        NextPacket { packets: self }
    }
}

impl Drop for Packets {
    fn drop(&mut self) {
        self.reactor.release(self.key);
    }
}

pub struct NextPacket<'a> {
    packets: &'a Packets,
}

impl Future for NextPacket<'_> {
    type Output = Packet;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Packet> {
        self.packets
            .reactor
            .poll_packet(self.packets.key, cx.waker())
    }
}
