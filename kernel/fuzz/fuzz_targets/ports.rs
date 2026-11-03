//! Runs operations on ports, on objects with signals and timelines with
//! values, and on the registrations of asynchronous waits on them, as
//! the kernel makes them, and checks the queues and registrations after
//! each operation: every registration that can still fire has room for
//! its packet, a registration is queued exactly while its packet is,
//! no wait misses signals or values that should have fired it, watches
//! end with their waits, user packets keep their order and charges, and
//! each packet read holds what its registration waited for.

#![no_main]

use kernel_core::port::{
    Origin, PACKET_SIZE, Queue, Registration, Registrations, SIGNAL, TIMELINE, Target, USER, Wait,
    packet,
};
use kernel_core::status::Error;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

const PORTS: usize = 4;
const OBJECTS: usize = 4;
const TIMELINES: usize = 2;
/// Timelines have object IDs after the objects with signals.
const FIRST_TIMELINE: u64 = OBJECTS as u64 + 1;

/// The charge of a user packet; the tokens alive count the charges held.
type Token = Rc<()>;
type Shared = Rc<RefCell<Option<Queue<Token>>>>;

/// A registration's port.
struct Port {
    index: usize,
    queue: Shared,
}

impl Target for Port {
    fn exists(&self) -> bool {
        self.queue.borrow().is_some()
    }

    fn deliver(&self, bytes: [u8; PACKET_SIZE], origin: Origin) {
        if let Some(q) = self.queue.borrow_mut().as_mut() {
            q.queue_registered(bytes, origin);
        }
    }
}

#[derive(Arbitrary, Debug)]
enum Op {
    NewPort,
    /// Drops the port, as when its last reference goes.
    DropPort(u8),
    Signal(u8, u32, u32),
    Raise(u8, u64),
    WaitAsync {
        port: u8,
        object: u8,
        process: u8,
        handle: u8,
        key: u8,
        mask: u32,
        edge: bool,
        refuse_charge: bool,
    },
    WaitTimeline {
        port: u8,
        timeline: u8,
        process: u8,
        handle: u8,
        key: u8,
        value: u64,
    },
    QueueUser(u8, u8, bool),
    Take(u8),
    HandleClosed(u8, Option<u8>),
    Cancel(u8, u8, u8, u8),
}

struct World {
    ports: Vec<Shared>,
    signals: [u32; OBJECTS],
    values: [u64; TIMELINES],
    /// The watches of each object with signals.
    watches: [usize; OBJECTS],
    registrations: Registrations<Port>,
    next_id: u64,
    token: Token,
    /// The keys of the user packets of each port, in queue order.
    users: Vec<VecDeque<u64>>,
}

/// Ends the registration's hold on its port's queue, as the kernel does
/// when it removes one.
fn release(r: &Registration<Port>) {
    if let Some(q) = r.extra.queue.borrow_mut().as_mut() {
        q.remove_registration();
    }
}

impl World {
    fn port(&self, index: u8) -> Option<(usize, Shared)> {
        let index = usize::from(index) % self.ports.len().max(1);
        let port = self.ports.get(index)?;
        port.borrow().is_some().then(|| (index, port.clone()))
    }

    /// Removes the registrations `matches` accepts, ending the watches
    /// of those that had not ended, and then those that ended.
    fn remove(&mut self, matches: impl Fn(&Registration<Port>) -> bool) {
        let mut removed = Vec::with_capacity(self.registrations.iter().count());
        self.registrations
            .take(matches, &mut removed, |_| unreachable!("room for all"));
        for r in &removed {
            if !r.is_done() && r.wait.threshold.is_none() {
                self.watches[r.wait.koid as usize - 1] -= 1;
            }
            release(r);
        }
        self.purge();
    }

    fn purge(&mut self) {
        let mut ended = Vec::with_capacity(self.registrations.iter().count());
        self.registrations.take(
            |r| r.is_done(),
            &mut ended,
            |_| unreachable!("room for all"),
        );
        ended.iter().for_each(release);
    }

    /// A registration through `handle` of `process` on the port, with its
    /// room reserved, as the kernel makes them.
    fn reserve(&mut self, port: u8, wait: Wait, refuse: bool) -> Option<Registration<Port>> {
        self.purge();
        let (index, queue) = self.port(port)?;
        let charge = || match refuse {
            true => Err(Error::NoResources),
            false => Ok(()),
        };
        queue.borrow_mut().as_mut()?.add_registration(charge).ok()?;
        self.next_id += 1;
        let wait = Wait {
            id: self.next_id,
            ..wait
        };
        Some(Registration::new(wait, Port { index, queue }))
    }

    fn take(&mut self, port: u8) {
        let Some((index, queue)) = self.port(port) else {
            return;
        };
        let Some(p) = queue.borrow_mut().as_mut().unwrap().take() else {
            assert!(self.users[index].is_empty());
            return;
        };
        let word = |at: usize| u64::from_le_bytes(p.bytes[at..at + 8].try_into().unwrap());
        let kind = word(8) as u32;
        let Some(origin) = p.origin else {
            assert_eq!(kind, USER);
            assert_eq!(self.users[index].pop_front(), Some(word(0)));
            return;
        };
        self.registrations.packet_read(origin.id);
        let Some(r) = self.registrations.iter().find(|r| r.wait.id == origin.id) else {
            return;
        };
        let w = &r.wait;
        assert_eq!(
            (word(0), origin.process, origin.handle),
            (w.key, w.process, w.handle)
        );
        match w.threshold {
            Some(threshold) => {
                assert_eq!(kind, TIMELINE);
                assert!(word(16) >= threshold);
            }
            None => {
                assert_eq!(kind, SIGNAL);
                let (mask, signals) = (word(16) as u32, (word(16) >> 32) as u32);
                assert_eq!(mask, w.mask);
                assert_ne!(signals & mask, 0);
            }
        }
    }

    fn apply(&mut self, op: &Op) {
        match *op {
            Op::NewPort if self.ports.len() < PORTS => {
                self.ports.push(Rc::new(RefCell::new(Some(Queue::new()))));
                self.users.push(VecDeque::new());
            }
            Op::NewPort => {}
            Op::DropPort(i) => {
                if let Some((index, queue)) = self.port(i) {
                    *queue.borrow_mut() = None;
                    self.users[index].clear();
                }
            }
            Op::Signal(object, clear, set) => {
                let i = usize::from(object) % OBJECTS;
                self.signals[i] = self.signals[i] & !clear | set;
                let ended = self
                    .registrations
                    .signals_changed(i as u64 + 1, self.signals[i]);
                self.watches[i] -= ended;
            }
            Op::Raise(timeline, by) => {
                let i = usize::from(timeline) % TIMELINES;
                self.values[i] = self.values[i].saturating_add(by % 8);
                let koid = FIRST_TIMELINE + i as u64;
                self.registrations.timeline_raised(koid, self.values[i]);
            }
            Op::WaitAsync {
                port,
                object,
                process,
                handle,
                key,
                mask,
                edge,
                refuse_charge,
            } => {
                let i = usize::from(object) % OBJECTS;
                let wait = Wait {
                    id: 0,
                    koid: i as u64 + 1,
                    mask,
                    edge,
                    key: u64::from(key % 4),
                    process: usize::from(process % 3),
                    handle: u32::from(handle % 4),
                    threshold: None,
                };
                let Some(r) = self.reserve(port, wait, refuse_charge) else {
                    return;
                };
                self.registrations.try_reserve().unwrap();
                self.watches[i] += 1;
                if let Some(ended) = self.registrations.add_signal_wait(r, self.signals[i]) {
                    self.watches[i] -= 1;
                    release(&ended);
                }
            }
            Op::WaitTimeline {
                port,
                timeline,
                process,
                handle,
                key,
                value,
            } => {
                let i = usize::from(timeline) % TIMELINES;
                let threshold = self.values[i].saturating_add(value % 8).saturating_sub(2);
                let wait = Wait {
                    id: 0,
                    koid: FIRST_TIMELINE + i as u64,
                    mask: 0,
                    edge: false,
                    key: u64::from(key % 4),
                    process: usize::from(process % 3),
                    handle: u32::from(handle % 4),
                    threshold: Some(threshold),
                };
                let Some(mut r) = self.reserve(port, wait, false) else {
                    return;
                };
                if self.values[i] >= threshold {
                    r.queue(TIMELINE, &self.values[i].to_le_bytes());
                    release(&r);
                } else {
                    self.registrations.try_reserve().unwrap();
                    self.registrations.push(r);
                }
            }
            Op::QueueUser(port, key, refuse) => {
                let Some((index, queue)) = self.port(port) else {
                    return;
                };
                let token = self.token.clone();
                let charge = || match refuse {
                    true => Err(Error::NoResources),
                    false => Ok(token),
                };
                let bytes = packet(u64::from(key), 0x77, 5, &[]);
                let queued = queue
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .queue_user(&bytes, charge);
                if queued.is_ok() {
                    self.users[index].push_back(u64::from(key));
                }
            }
            Op::Take(port) => self.take(port),
            Op::HandleClosed(process, handle) => {
                let process = usize::from(process % 3);
                let handle = handle.map(|h| u32::from(h % 4));
                self.remove(|r| {
                    r.wait.process == process && handle.is_none_or(|h| h == r.wait.handle)
                });
            }
            Op::Cancel(port, process, handle, key) => {
                let Some((index, queue)) = self.port(port) else {
                    return;
                };
                let (process, handle, key) = (
                    usize::from(process % 3),
                    u32::from(handle % 4),
                    u64::from(key % 4),
                );
                self.remove(|r| {
                    let w = &r.wait;
                    r.extra.index == index
                        && w.process == process
                        && w.handle == handle
                        && w.key == key
                });
                queue
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .cancel(process, handle, key);
            }
        }
    }

    fn check(&self) {
        let mut charged = 0;
        for (index, port) in self.ports.iter().enumerate() {
            let port = port.borrow();
            let Some(queue) = port.as_ref() else {
                continue;
            };
            let ours: Vec<_> = self
                .registrations
                .iter()
                .filter(|r| r.extra.index == index)
                .collect();
            assert_eq!(queue.registrations(), ours.len());
            let waiting = ours
                .iter()
                .filter(|r| !r.is_done() && !r.is_queued())
                .count();
            assert!(
                queue.spare() >= waiting,
                "no room for a waiting registration's packet"
            );
            for r in &ours {
                let packets = queue
                    .packets()
                    .filter(|p| p.origin.is_some_and(|o| o.id == r.wait.id))
                    .count();
                assert_eq!(packets, usize::from(r.is_queued()));
            }
            let users: Vec<u64> = queue
                .packets()
                .filter(|p| p.origin.is_none())
                .map(|p| u64::from_le_bytes(p.bytes[..8].try_into().unwrap()))
                .collect();
            assert!(users.iter().eq(self.users[index].iter()));
            charged += users.len();
        }
        assert_eq!(Rc::strong_count(&self.token) - 1, charged);
        let mut watches = [0; OBJECTS];
        for r in self.registrations.iter().filter(|r| !r.is_done()) {
            let w = &r.wait;
            match w.threshold {
                Some(threshold) => {
                    let value = self.values[(w.koid - FIRST_TIMELINE) as usize];
                    assert!(value < threshold, "a timeline wait missed its value");
                }
                None => {
                    let signals = self.signals[w.koid as usize - 1] & w.mask;
                    watches[w.koid as usize - 1] += 1;
                    if w.edge {
                        assert_eq!(r.last(), signals);
                    } else {
                        assert_eq!(signals, 0, "a wait missed its signals");
                    }
                }
            }
        }
        assert_eq!(watches, self.watches);
    }
}

fuzz_target!(|ops: Vec<Op>| {
    let mut world = World {
        ports: Vec::new(),
        signals: [0; OBJECTS],
        values: [0; TIMELINES],
        watches: [0; OBJECTS],
        registrations: Registrations::new(),
        next_id: 0,
        token: Rc::new(()),
        users: Vec::new(),
    };
    for op in ops.iter().take(256) {
        world.apply(op);
        world.check();
    }
});
