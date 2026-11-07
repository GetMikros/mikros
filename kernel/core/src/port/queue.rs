//! The bookkeeping of ports: their queues of packets, and the
//! registrations of the asynchronous waits that fill them.

use crate::object::handles::Handle;
use crate::status::Error;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// The size of a packet.
pub const PACKET_SIZE: usize = 48;
/// Packet types.
pub const SIGNAL: u32 = 1;
pub const USER: u32 = 2;
pub const TIMELINE: u32 = 4;
/// The `EDGE` option of `object_wait_async`.
pub const EDGE: u32 = 1 << 0;
/// The most `USER` packets queued on a port.
pub const USER_MAX: usize = 4096;
/// The most registrations of a port.
pub const REGISTRATIONS_MAX: usize = 65536;

/// The bytes of a packet.
pub fn packet(key: u64, kind: u32, status: i32, payload: &[u8]) -> [u8; PACKET_SIZE] {
    let mut p = [0u8; PACKET_SIZE];
    p[0x00..0x08].copy_from_slice(&key.to_le_bytes());
    p[0x08..0x0C].copy_from_slice(&kind.to_le_bytes());
    p[0x0C..0x10].copy_from_slice(&status.to_le_bytes());
    p[0x10..0x10 + payload.len()].copy_from_slice(payload);
    p
}

/// The registration that queued a packet, and the process and handle it
/// was made through, which outlive it for a one-shot registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    pub id: u64,
    pub process: usize,
    pub handle: Handle,
}

/// A queued packet, with the registration that queued it, or the charge
/// that a packet without one holds until it is read.
pub struct Packet<C> {
    pub bytes: [u8; PACKET_SIZE],
    pub origin: Option<Origin>,
    pub charge: Option<C>,
}

/// The packets of a port. `C` is the charge of a packet that no
/// registration queued.
pub struct Queue<C> {
    packets: VecDeque<Packet<C>>,
    /// The live registrations on the port; each may have one packet
    /// queued, for which the queue keeps room.
    registrations: usize,
    /// The packets queued without a registration.
    users: usize,
}

impl<C> Default for Queue<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> Queue<C> {
    pub const fn new() -> Queue<C> {
        Queue {
            packets: VecDeque::new(),
            registrations: 0,
            users: 0,
        }
    }

    /// Makes room for one packet beyond those of the registrations.
    fn room(&mut self) -> Result<(), Error> {
        let room = self.registrations + 1;
        self.packets.try_reserve(room).map_err(|_| Error::NoMemory)
    }

    /// Adds a registration, keeping room for its packet, once `charge`
    /// pays for it. Fails with `NoResources` when the port has the most
    /// registrations, with `NoMemory` without room, or as `charge` does,
    /// in each case adding none.
    pub fn add_registration(
        &mut self,
        charge: impl FnOnce() -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.registrations >= REGISTRATIONS_MAX {
            return Err(Error::NoResources);
        }
        self.room()?;
        charge()?;
        self.registrations += 1;
        Ok(())
    }

    /// Removes a registration that `add_registration` added.
    pub fn remove_registration(&mut self) {
        debug_assert!(self.registrations > 0);
        self.registrations -= 1;
    }

    /// The number of registrations.
    pub fn registrations(&self) -> usize {
        self.registrations
    }

    /// Queues a user packet, as `port_queue` does, with the charge that
    /// `charge` makes. Fails with `ShouldWait` when the port holds the
    /// most user packets, with `NoMemory` without room, or as `charge`
    /// does.
    pub fn queue_user(
        &mut self,
        bytes: &[u8; PACKET_SIZE],
        charge: impl FnOnce() -> Result<C, Error>,
    ) -> Result<(), Error> {
        if self.users >= USER_MAX {
            return Err(Error::ShouldWait);
        }
        let mut copy = *bytes;
        copy[0x08..0x0C].copy_from_slice(&USER.to_le_bytes());
        copy[0x0C..0x10].fill(0);
        self.queue_kernel(copy, charge)
    }

    /// Queues a packet that the kernel makes, such as a page request,
    /// with the charge that `charge` makes. Fails with `NoMemory` without
    /// room or as `charge` does.
    pub fn queue_kernel(
        &mut self,
        bytes: [u8; PACKET_SIZE],
        charge: impl FnOnce() -> Result<C, Error>,
    ) -> Result<(), Error> {
        self.room()?;
        let charge = charge()?;
        // A kernel packet counts as a user packet until read.
        self.users += 1;
        self.packets.push_back(Packet {
            bytes,
            origin: None,
            charge: Some(charge),
        });
        Ok(())
    }

    /// Queues the packet of the registration `origin`, for which the
    /// queue keeps room. Allocates nothing.
    pub fn queue_registered(&mut self, bytes: [u8; PACKET_SIZE], origin: Origin) {
        debug_assert!(self.packets.len() < self.packets.capacity());
        self.packets.push_back(Packet {
            bytes,
            origin: Some(origin),
            charge: None,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    /// The packets in queue order.
    pub fn packets(&self) -> impl Iterator<Item = &Packet<C>> {
        self.packets.iter()
    }

    /// The number of packets the queue has room for without allocating.
    pub fn spare(&self) -> usize {
        self.packets.capacity() - self.packets.len()
    }

    /// Removes the oldest packet, if any.
    pub fn take(&mut self) -> Option<Packet<C>> {
        let packet = self.packets.pop_front()?;
        if packet.origin.is_none() {
            self.users -= 1;
        }
        Some(packet)
    }

    /// Removes the unread packets that registrations made through
    /// `handle` of `process` queued with `key`.
    pub fn cancel(&mut self, process: usize, handle: Handle, key: u64) {
        self.packets.retain(|p| {
            let packet_key = u64::from_le_bytes(p.bytes[0x00..0x08].try_into().expect("8 bytes"));
            p.origin
                .is_none_or(|o| !(o.process == process && o.handle == handle && packet_key == key))
        });
    }
}

/// What an asynchronous wait waits for, and where its packet goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    pub id: u64,
    /// The object waited on.
    pub koid: u64,
    /// The signals waited for, for a wait on signals.
    pub mask: u32,
    pub edge: bool,
    pub key: u64,
    /// The process and handle the wait was made through.
    pub process: usize,
    pub handle: Handle,
    /// For a wait on a timeline, the value it waits for; such a wait does
    /// not watch signals.
    pub threshold: Option<u64>,
}

/// The port of a registration, as the kernel holds it.
pub trait Target {
    /// Whether the port still exists.
    fn exists(&self) -> bool;
    /// Queues `bytes` of the registration `origin` on the port, for which
    /// it keeps room, if the port still exists. Returns the port's object
    /// ID and signals when the packet changed them, for the waits on the
    /// port, which the caller fires.
    fn deliver(&self, bytes: [u8; PACKET_SIZE], origin: Origin) -> Option<(u64, u32)>;
}

/// An asynchronous wait, with `extra`, what the kernel keeps with it:
/// its port, and what it needs to end the wait.
pub struct Registration<X> {
    pub wait: Wait,
    /// The signals of the mask last seen, for `EDGE`.
    last: u32,
    /// Whether a packet of the registration is queued and unread.
    queued: bool,
    /// Set once the wait has ended, and its watch with it, until the
    /// registration is removed.
    done: bool,
    /// Set for a wait that ended while the waits on another object fired,
    /// whose watch then ends when the registration is removed.
    watch_ends_later: bool,
    /// The object ID and signals of the port that the registration's
    /// packet made readable, whose waits are still to fire.
    cascade: Option<(u64, u32)>,
    pub extra: X,
}

impl<X> Registration<X> {
    pub fn new(wait: Wait, extra: X) -> Registration<X> {
        Registration {
            wait,
            last: 0,
            queued: false,
            done: false,
            watch_ends_later: false,
            cascade: None,
            extra,
        }
    }
}

impl<X: Target> Registration<X> {
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Whether a packet of the registration is queued and unread.
    pub fn is_queued(&self) -> bool {
        self.queued
    }

    /// Whether the wait ended without ending its watch, which then ends
    /// when the registration is removed.
    pub fn watch_ends_later(&self) -> bool {
        self.watch_ends_later
    }

    /// The port that the registration's packet made readable, if its
    /// waits are still to fire.
    pub fn take_cascade(&mut self) -> Option<(u64, u32)> {
        self.cascade.take()
    }

    /// The signals of the mask last seen.
    pub fn last(&self) -> u32 {
        self.last
    }

    /// Queues a packet of `kind` with `payload` on the port, unless one
    /// is queued. Returns whether the port still exists.
    pub fn queue(&mut self, kind: u32, payload: &[u8]) -> bool {
        if !self.extra.exists() {
            return false;
        }
        if !core::mem::replace(&mut self.queued, true) {
            let w = &self.wait;
            let origin = Origin {
                id: w.id,
                process: w.process,
                handle: w.handle,
            };
            if let Some(cascade) = self.extra.deliver(packet(w.key, kind, 0, payload), origin) {
                self.cascade = Some(cascade);
            }
        }
        true
    }

    /// Queues a `SIGNAL` packet for `signals`, unless one is queued.
    /// Returns whether the port still exists.
    fn fire(&mut self, signals: u32) -> bool {
        let mut payload = [0u8; 8];
        payload[0..4].copy_from_slice(&self.wait.mask.to_le_bytes());
        payload[4..8].copy_from_slice(&signals.to_le_bytes());
        self.queue(SIGNAL, &payload)
    }
}

/// The registrations of the waits that have not been removed.
pub struct Registrations<X> {
    list: Vec<Registration<X>>,
    /// How many of them have ended.
    ended: usize,
}

impl<X> Default for Registrations<X> {
    fn default() -> Self {
        Self::new()
    }
}

impl<X> Registrations<X> {
    pub const fn new() -> Registrations<X> {
        Registrations {
            list: Vec::new(),
            ended: 0,
        }
    }
}

impl<X: Target> Registrations<X> {
    pub fn iter(&self) -> impl Iterator<Item = &Registration<X>> {
        self.list.iter()
    }

    /// Whether any of them has ended, so that removing the ended ones
    /// finds some.
    pub fn any_ended(&self) -> bool {
        self.ended > 0
    }

    /// Makes room for one more registration.
    pub fn try_reserve(&mut self) -> Result<(), Error> {
        self.list.try_reserve(1).map_err(|_| Error::NoMemory)
    }

    /// Adds `registration`, for which `try_reserve` made room. Allocates
    /// nothing.
    pub fn push(&mut self, registration: Registration<X>) {
        debug_assert!(self.list.len() < self.list.capacity());
        self.list.push(registration);
    }

    /// Starts the wait of `registration` on signals, on an object whose
    /// signals are `signals` and which it watches: queues its packet at
    /// once when `signals` match the mask, and then gives it back, ended,
    /// unless it is an `EDGE` wait. Otherwise adds it, for which
    /// `try_reserve` made room.
    pub fn add_signal_wait(
        &mut self,
        mut registration: Registration<X>,
        signals: u32,
    ) -> Option<Registration<X>> {
        let now = signals & registration.wait.mask;
        registration.last = now;
        if now != 0 {
            registration.fire(signals);
        }
        let cascade = registration.cascade.take();
        let ended = now != 0 && !registration.wait.edge;
        let back = if ended {
            registration.done = true;
            Some(registration)
        } else {
            self.push(registration);
            None
        };
        self.cascade(cascade);
        back
    }

    /// Fires the waits on the object with ID `koid`, whose signals are now
    /// `signals`. A wait without `EDGE` ends once it fires; an `EDGE` wait
    /// fires on each signal of its mask that is newly set, and ends once
    /// its port is gone. Returns how many watches of the object ended.
    /// Allocates and drops nothing.
    /// Then fires the waits on the ports that their packets made readable,
    /// as `cascade` does.
    pub fn signals_changed(&mut self, koid: u64, signals: u32) -> usize {
        let ended = self.fire(koid, signals, false);
        self.cascade(None);
        ended
    }

    /// Fires the waits on the object `koid` as `signals_changed` does, and
    /// returns how many ended, which with `later` end their watches only
    /// when they are removed.
    fn fire(&mut self, koid: u64, signals: u32, later: bool) -> usize {
        let mut ended = 0;
        for r in &mut self.list {
            if r.wait.koid != koid || r.done || r.wait.threshold.is_some() {
                continue;
            }
            let now = signals & r.wait.mask;
            let end = if r.wait.edge {
                let before = core::mem::replace(&mut r.last, now);
                now & !before != 0 && !r.fire(signals)
            } else {
                now != 0 && {
                    r.fire(signals);
                    true
                }
            };
            if end {
                r.done = true;
                r.watch_ends_later = later;
                ended += 1;
            }
        }
        self.ended += ended;
        ended
    }

    /// Fires the waits on the ports that queued packets made readable,
    /// `first` and then those the registrations recorded, until none is
    /// left. A registration queues at most one packet until it is read,
    /// so this ends, also when ports wait on each other. Allocates and
    /// drops nothing.
    fn cascade(&mut self, mut next: Option<(u64, u32)>) {
        loop {
            let found = next
                .take()
                .or_else(|| self.list.iter_mut().find_map(|r| r.cascade.take()));
            let Some((koid, signals)) = found else {
                return;
            };
            self.fire(koid, signals, true);
        }
    }

    /// Queues the packets of the waits on the timeline with object ID
    /// `koid` that its new value `value` reaches, and ends them.
    pub fn timeline_raised(&mut self, koid: u64, value: u64) {
        for r in &mut self.list {
            let reached = r.wait.threshold.is_some_and(|t| t <= value);
            if r.wait.koid == koid && reached && !r.done {
                r.queue(TIMELINE, &value.to_le_bytes());
                r.done = true;
                self.ended += 1;
            }
        }
        self.cascade(None);
    }

    /// Lets the registration with `id` queue again, as its packet was
    /// read.
    pub fn packet_read(&mut self, id: u64) {
        for r in &mut self.list {
            if r.wait.id == id {
                r.queued = false;
            }
        }
    }

    /// Removes the registrations that `remove` accepts, moving them to
    /// `out` as far as it has room and handing the others to `overflow`.
    /// Allocates nothing.
    pub fn take(
        &mut self,
        remove: impl Fn(&Registration<X>) -> bool,
        out: &mut Vec<Registration<X>>,
        mut overflow: impl FnMut(Registration<X>),
    ) {
        let mut i = 0;
        while i < self.list.len() {
            if !remove(&self.list[i]) {
                i += 1;
                continue;
            }
            let r = self.list.swap_remove(i);
            if r.done {
                self.ended -= 1;
            }
            if out.len() < out.capacity() {
                out.push(r);
            } else {
                overflow(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use std::rc::Rc;
    use std::vec;

    /// A port shared with the registrations on it.
    #[derive(Clone)]
    struct Shared(Rc<RefCell<Option<Queue<()>>>>);

    impl Target for Shared {
        fn exists(&self) -> bool {
            self.0.borrow().is_some()
        }

        fn deliver(&self, bytes: [u8; PACKET_SIZE], origin: Origin) -> Option<(u64, u32)> {
            let mut q = self.0.borrow_mut();
            let q = q.as_mut()?;
            let was_empty = q.is_empty();
            q.queue_registered(bytes, origin);
            // The test ports have the object ID 9.
            was_empty.then_some((9, 1))
        }
    }

    fn wait(id: u64, mask: u32, edge: bool) -> Wait {
        Wait {
            id,
            koid: 7,
            mask,
            edge,
            key: id * 10,
            process: 1,
            handle: 0x100,
            threshold: None,
        }
    }

    fn port() -> Shared {
        Shared(Rc::new(RefCell::new(Some(Queue::new()))))
    }

    fn register(list: &mut Registrations<Shared>, port: &Shared, w: Wait, signals: u32) -> bool {
        port.0
            .borrow_mut()
            .as_mut()
            .unwrap()
            .add_registration(|| Ok(()))
            .unwrap();
        list.try_reserve().unwrap();
        list.add_signal_wait(Registration::new(w, port.clone()), signals)
            .is_none()
    }

    fn keys(port: &Shared) -> std::vec::Vec<u64> {
        let mut q = port.0.borrow_mut();
        let q = q.as_mut().unwrap();
        core::iter::from_fn(|| q.take())
            .map(|p| u64::from_le_bytes(p.bytes[..8].try_into().unwrap()))
            .collect()
    }

    #[test]
    fn fires_level_waits_once_and_edge_waits_on_new_signals() {
        let (mut list, port) = (Registrations::new(), port());
        assert!(!register(&mut list, &port, wait(1, 0b01, false), 0b01));
        assert!(register(&mut list, &port, wait(2, 0b10, false), 0));
        assert!(register(&mut list, &port, wait(3, 0b10, true), 0));
        assert_eq!(keys(&port), [10]);
        assert_eq!(list.signals_changed(7, 0b10), 1);
        assert_eq!(list.signals_changed(7, 0b10), 0);
        assert_eq!(keys(&port), [20, 30]);
        list.packet_read(3);
        list.signals_changed(7, 0);
        list.signals_changed(7, 0b10);
        assert_eq!(keys(&port), [30]);
    }

    #[test]
    fn fires_the_waits_on_the_ports_that_packets_make_readable() {
        let (mut list, a, b) = (Registrations::new(), port(), port());
        // Object 7 is waited on into port `a`. The test ports all have
        // the object ID 9: an edge wait and a one-shot wait on it queue
        // into `b`, and an edge wait on it into `a`, so the ports wait
        // on each other.
        let on_ports = |id, edge| Wait {
            koid: 9,
            ..wait(id, 1, edge)
        };
        assert!(register(&mut list, &a, wait(1, 1, true), 0));
        assert!(register(&mut list, &b, on_ports(2, true), 0));
        assert!(register(&mut list, &b, on_ports(3, false), 0));
        assert!(register(&mut list, &a, on_ports(4, true), 0));
        assert_eq!(list.signals_changed(7, 1), 0);
        assert_eq!(keys(&a), [10, 40]);
        assert_eq!(keys(&b), [20, 30]);
        // The one-shot wait ended while the waits on 7 fired, so its
        // watch of 9 ends later.
        let ended: std::vec::Vec<_> = list.iter().filter(|r| r.is_done()).collect();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].wait.id, 3);
        assert!(ended[0].watch_ends_later());
    }

    #[test]
    fn ends_edge_waits_once_their_port_is_gone() {
        let (mut list, port) = (Registrations::new(), port());
        assert!(register(&mut list, &port, wait(1, 1, true), 0));
        *port.0.borrow_mut() = None;
        assert_eq!(list.signals_changed(7, 1), 1);
        let mut out = vec::Vec::with_capacity(1);
        list.take(|r| r.is_done(), &mut out, |_| panic!("no room"));
        assert_eq!(out.len(), 1);
        assert_eq!(list.iter().count(), 0);
    }

    #[test]
    fn counts_the_ended_registrations_until_they_are_taken() {
        let (mut list, port) = (Registrations::new(), port());
        assert!(register(&mut list, &port, wait(1, 1, false), 0));
        assert!(register(&mut list, &port, wait(2, 2, false), 0));
        assert!(!list.any_ended());
        list.signals_changed(7, 1);
        assert!(list.any_ended());
        let mut out = vec::Vec::with_capacity(1);
        list.take(|r| r.is_done(), &mut out, |_| panic!("no room"));
        assert!(!list.any_ended());
        assert_eq!(list.iter().count(), 1);
    }

    #[test]
    fn limits_user_packets_and_cancels_registered_ones() {
        let mut q = Queue::<u8>::new();
        let bytes = packet(5, 9, 3, &[]);
        for _ in 0..USER_MAX {
            q.queue_user(&bytes, || Ok(1)).unwrap();
        }
        assert_eq!(q.queue_user(&bytes, || Ok(1)), Err(Error::ShouldWait));
        let p = q.take().unwrap();
        assert_eq!(&p.bytes[8..16], &[2, 0, 0, 0, 0, 0, 0, 0]);
        q.add_registration(|| Ok(())).unwrap();
        let origin = Origin {
            id: 1,
            process: 4,
            handle: 0x100,
        };
        q.queue_registered(packet(6, SIGNAL, 0, &[]), origin);
        q.cancel(4, 0x100, 6);
        assert_eq!(q.packets().count(), USER_MAX - 1);
    }
}
