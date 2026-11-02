//! Channels: two endpoints, each with a queue of messages written to the
//! other.

use super::budget::Budget;
use super::handles::Entry;
use super::{Header, Object, ObjectType, signals};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

/// The most bytes in one message.
pub const BYTES_MAX: usize = 65536;
/// The most handles in one message.
pub const HANDLES_MAX: usize = 64;
/// A queue is full when it holds this many messages.
const QUEUE_MESSAGES: usize = 128;
/// A queue is full when it holds this many bytes of message data.
const QUEUE_BYTES: usize = 524288;

/// A queued message, charged to the budget of the process that wrote it
/// until it is read or discarded.
pub struct Message {
    pub bytes: Vec<u8>,
    pub handles: Vec<Entry>,
    charged: Option<(Arc<Budget>, u64)>,
}

impl Message {
    /// A message holding `bytes` and `handles`. Its kernel memory is
    /// charged to `budget`, if given.
    pub fn new(
        bytes: Vec<u8>,
        handles: Vec<Entry>,
        budget: Option<&Arc<Budget>>,
    ) -> Result<Message, Error> {
        let charged = match budget {
            Some(budget) => {
                let cost = (size_of::<Message>()
                    + bytes.capacity()
                    + handles.capacity() * size_of::<Entry>()) as u64;
                budget.charge_kernel(cost)?;
                Some((budget.clone(), cost))
            }
            None => None,
        };
        Ok(Message {
            bytes,
            handles,
            charged,
        })
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        if let Some((budget, cost)) = &self.charged {
            budget.release_kernel(*cost);
        }
    }
}

/// The bit that marks transaction IDs of `channel_call`.
pub const CALL_ID: u32 = 1 << 31;

/// An outstanding `channel_call`, with its reply once it arrives.
struct Call {
    id: u32,
    reply: Option<Message>,
}

#[derive(Default)]
struct Queue {
    messages: VecDeque<Message>,
    bytes: usize,
    /// The calls made through the endpoint the queue leads to.
    calls: Vec<Call>,
    /// The IDs of calls that ended without their reply, whose late
    /// replies are discarded.
    abandoned: Vec<u32>,
    /// The next transaction ID to try, without `CALL_ID`.
    next_id: u32,
}

impl Queue {
    fn full(&self) -> bool {
        self.messages.len() >= QUEUE_MESSAGES || self.bytes >= QUEUE_BYTES
    }
}

/// The state both endpoints share. Queue `i` holds the messages toward
/// endpoint `i`.
struct Shared {
    queues: [Queue; 2],
    ends: [Weak<Endpoint>; 2],
    /// The budget charged `CHANNEL_COST` for the channel.
    charged: Option<Arc<Budget>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(budget) = &self.charged {
            budget.release_kernel(CHANNEL_COST);
        }
    }
}

/// The kernel memory charged for a channel: its endpoints and the state
/// they share.
pub const CHANNEL_COST: u64 = (2 * size_of::<Endpoint>() + size_of::<SpinLock<Shared>>()) as u64;

pub struct Endpoint {
    header: Header,
    side: usize,
    shared: Arc<SpinLock<Shared>>,
    /// Told when the peer is destroyed.
    observer: SpinLock<Option<Weak<dyn PeerObserver>>>,
}

/// What the kernel does when the peer of an endpoint it holds is
/// destroyed. It is called with no channel lock held, from wherever the
/// peer's last reference is dropped, so it must not take locks that such
/// a place may hold, such as a handle table's.
pub trait PeerObserver: Send + Sync {
    fn peer_closed(&self);
}

/// What `Endpoint::read` found when the first message did not fit.
#[derive(Debug, PartialEq, Eq)]
pub struct Needed {
    pub len: u64,
    pub count: u64,
}

impl Endpoint {
    /// A new channel's two endpoints, both `WRITABLE`. `CHANNEL_COST` is
    /// charged to `budget`, if given, until both are destroyed.
    pub fn pair(budget: Option<&Arc<Budget>>) -> Result<(Arc<Endpoint>, Arc<Endpoint>), Error> {
        if let Some(budget) = budget {
            budget.charge_kernel(CHANNEL_COST)?;
        }
        let shared = Arc::new(SpinLock::new(Shared {
            queues: [Queue::default(), Queue::default()],
            ends: [Weak::new(), Weak::new()],
            charged: budget.cloned(),
        }));
        let end = |side| {
            let header = Header::new(ObjectType::Channel);
            header.update_signals(0, signals::WRITABLE);
            Arc::new(Endpoint {
                header,
                side,
                shared: shared.clone(),
                observer: SpinLock::new(None),
            })
        };
        let (a, b) = (end(0), end(1));
        shared.lock().ends = [Arc::downgrade(&a), Arc::downgrade(&b)];
        Ok((a, b))
    }

    /// Makes `observer` be told when the peer is destroyed.
    pub fn set_observer(&self, observer: Weak<dyn PeerObserver>) {
        *self.observer.lock() = Some(observer);
    }

    /// Whether the peer has been destroyed.
    pub fn peer_closed(&self) -> bool {
        self.header.signals() & signals::PEER_CLOSED != 0
    }

    fn peer_side(&self) -> usize {
        1 - self.side
    }

    /// Queues `message` toward the peer. Fails with `PeerClosed` when the
    /// peer is gone and `ShouldWait` when its queue is full, giving the
    /// message back.
    ///
    /// A message whose transaction ID belongs to an outstanding call of
    /// the peer goes to that call instead of the queue, and the peer's
    /// object ID and the ID are returned so that the caller can be woken.
    /// One whose ID belongs to a call that ended without its reply is
    /// discarded.
    pub fn write(&self, message: Message) -> Result<Option<(u64, u32)>, (Error, Message)> {
        let mut shared = self.shared.lock();
        let peer = shared.ends[self.peer_side()].clone();
        if peer.strong_count() == 0 {
            return Err((Error::PeerClosed, message));
        }
        let queue = &mut shared.queues[self.peer_side()];
        let id = message
            .bytes
            .get(..4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
            .filter(|id| id & CALL_ID != 0);
        if let Some(id) = id {
            if let Some(call) = queue
                .calls
                .iter_mut()
                .find(|c| c.id == id && c.reply.is_none())
            {
                call.reply = Some(message);
                drop(shared);
                let koid = peer.upgrade().map_or(0, |p| p.header.koid());
                return Ok(Some((koid, id)));
            }
            if let Some(i) = queue.abandoned.iter().position(|&a| a == id) {
                queue.abandoned.swap_remove(i);
                // The message and its handles are dropped with no lock
                // held, as that may destroy objects.
                drop(shared);
                drop(message);
                return Ok(None);
            }
        }
        if queue.full() {
            return Err((Error::ShouldWait, message));
        }
        if queue.messages.try_reserve(1).is_err() {
            return Err((Error::NoMemory, message));
        }
        queue.bytes += message.bytes.len();
        queue.messages.push_back(message);
        if queue.full() {
            self.header.update_signals(signals::WRITABLE, 0);
        }
        // The peer is set READABLE while the lock is held, so a reader
        // cannot clear it before it is set. Dropping the upgraded peer
        // may destroy it, which takes the lock, so that waits.
        let peer = peer.upgrade();
        if let Some(peer) = &peer {
            peer.header.update_signals(0, signals::READABLE);
        }
        drop(shared);
        drop(peer);
        Ok(None)
    }

    /// Starts a call through this endpoint and returns its transaction
    /// ID: one with `CALL_ID` set that no outstanding call and no call
    /// ended without its reply uses.
    pub fn begin_call(&self) -> Result<u32, Error> {
        let mut shared = self.shared.lock();
        let queue = &mut shared.queues[self.side];
        queue.calls.try_reserve(1).map_err(|_| Error::NoMemory)?;
        // Room to record every outstanding call as abandoned, so that
        // `abandon_call` never allocates.
        queue
            .abandoned
            .try_reserve(queue.calls.len() + 1)
            .map_err(|_| Error::NoMemory)?;
        let id = loop {
            let id = CALL_ID | (queue.next_id & !CALL_ID);
            queue.next_id = queue.next_id.wrapping_add(1);
            let used = queue.calls.iter().any(|c| c.id == id) || queue.abandoned.contains(&id);
            if !used {
                break id;
            }
        };
        queue.calls.push(Call { id, reply: None });
        Ok(id)
    }

    /// Ends call `id` and returns its reply, if it has arrived; otherwise
    /// the call stays outstanding and `None` is returned.
    pub fn take_reply(&self, id: u32) -> Option<Message> {
        let mut shared = self.shared.lock();
        let calls = &mut shared.queues[self.side].calls;
        let i = calls.iter().position(|c| c.id == id && c.reply.is_some())?;
        calls.swap_remove(i).reply
    }

    /// Ends call `id` without its reply: a reply arriving later is
    /// discarded. Returns a reply that arrived meanwhile, which the
    /// caller drops with no lock held.
    pub fn abandon_call(&self, id: u32) -> Option<Message> {
        let mut shared = self.shared.lock();
        let queue = &mut shared.queues[self.side];
        let i = queue.calls.iter().position(|c| c.id == id)?;
        let call = queue.calls.swap_remove(i);
        if call.reply.is_none() {
            debug_assert!(queue.abandoned.len() < queue.abandoned.capacity());
            queue.abandoned.push(id);
        }
        call.reply
    }

    /// The number of bytes and handles of the oldest message toward this
    /// endpoint, which stays queued. Fails as `take` does when there is
    /// none.
    pub fn front_sizes(&self) -> Result<(u64, u64), Error> {
        let shared = self.shared.lock();
        let peer = &shared.ends[self.peer_side()];
        match shared.queues[self.side].messages.front() {
            Some(m) => Ok((m.bytes.len() as u64, m.handles.len() as u64)),
            None if peer.strong_count() == 0 => Err(Error::PeerClosed),
            None => Err(Error::ShouldWait),
        }
    }

    /// Passes the oldest message toward this endpoint to `accept`, which
    /// gets its bytes and handles. When `accept` succeeds, the message is
    /// removed. Fails with `ShouldWait` or `PeerClosed` when there is
    /// none, and with what `accept` returns otherwise, leaving the message
    /// queued.
    pub fn read<E: From<Error>>(
        &self,
        accept: impl FnOnce(&[u8], &[Entry]) -> Result<(), E>,
    ) -> Result<(), E> {
        // The handles were installed by `accept`; dropping the message
        // drops only its own references.
        self.take(accept).map(drop)
    }

    /// Like `read`, but returns what `accept` returned together with the
    /// removed message, which keeps its charge until it is dropped.
    pub fn take<T, E: From<Error>>(
        &self,
        accept: impl FnOnce(&[u8], &[Entry]) -> Result<T, E>,
    ) -> Result<(T, Message), E> {
        let mut shared = self.shared.lock();
        let peer = shared.ends[self.peer_side()].clone();
        let queue = &mut shared.queues[self.side];
        let Some(message) = queue.messages.front() else {
            return Err(match peer.strong_count() {
                0 => Error::PeerClosed,
                _ => Error::ShouldWait,
            }
            .into());
        };
        let accepted = accept(&message.bytes, &message.handles)?;
        let message = queue.messages.pop_front().expect("checked above");
        queue.bytes -= message.bytes.len();
        if queue.messages.is_empty() {
            self.header.update_signals(signals::READABLE, 0);
        }
        let room = !queue.full();
        let peer = peer.upgrade();
        if let Some(peer) = &peer
            && room
        {
            peer.header.update_signals(0, signals::WRITABLE);
        }
        drop(shared);
        drop(peer);
        Ok((accepted, message))
    }
}

impl Object for Endpoint {
    fn header(&self) -> &Header {
        &self.header
    }

    fn related_koid(&self) -> u64 {
        let peer = self.shared.lock().ends[self.peer_side()].clone();
        peer.upgrade().map_or(0, |peer| peer.header.koid())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let (discarded, peer) = {
            let mut shared = self.shared.lock();
            let queue = core::mem::take(&mut shared.queues[self.side]);
            (queue, shared.ends[self.peer_side()].clone())
        };
        if let Some(peer) = peer.upgrade() {
            peer.header
                .update_signals(signals::WRITABLE, signals::PEER_CLOSED);
            let observer = peer.observer.lock().clone();
            if let Some(observer) = observer.and_then(|o| o.upgrade()) {
                observer.peer_closed();
            }
        }
        // Discarded messages close their handles, which can destroy
        // further objects, so this happens with no lock held.
        drop(discarded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::tests::Plain;
    use std::vec;

    fn message(bytes: &[u8], handles: Vec<Entry>) -> Message {
        Message::new(bytes.to_vec(), handles, None).unwrap()
    }

    fn read(end: &Endpoint) -> Result<(Vec<u8>, usize), Error> {
        let mut got = None;
        end.read::<Error>(|b, h| {
            got = Some((b.to_vec(), h.len()));
            Ok(())
        })?;
        Ok(got.unwrap())
    }

    #[test]
    fn delivers_replies_to_their_calls() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let (one, two) = (a.begin_call().unwrap(), a.begin_call().unwrap());
        assert!(one & CALL_ID != 0 && two & CALL_ID != 0 && one != two);
        let mut reply = two.to_le_bytes().to_vec();
        reply.push(9);
        assert_eq!(
            b.write(message(&reply, vec![])).ok(),
            Some(Some((a.header().koid(), two)))
        );
        // A reply is not queued and does not make the caller readable.
        assert_eq!(read(&a), Err(Error::ShouldWait));
        assert_eq!(a.header().signals() & signals::READABLE, 0);
        assert!(a.take_reply(one).is_none());
        assert_eq!(a.take_reply(two).unwrap().bytes, reply);
        // Other IDs with the bit set are ordinary messages.
        let other = (CALL_ID | 77).to_le_bytes();
        assert_eq!(b.write(message(&other, vec![])).ok(), Some(None));
        assert_eq!(read(&a).unwrap().0, other);
    }

    #[test]
    fn discards_replies_to_abandoned_calls() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let id = a.begin_call().unwrap();
        assert!(a.abandon_call(id).is_none());
        let next = a.begin_call().unwrap();
        assert_ne!(next, id);
        assert_eq!(b.write(message(&id.to_le_bytes(), vec![])).ok(), Some(None));
        assert_eq!(read(&a), Err(Error::ShouldWait));
        // Only the first late reply is discarded.
        assert_eq!(b.write(message(&id.to_le_bytes(), vec![])).ok(), Some(None));
        assert_eq!(read(&a).unwrap().0, id.to_le_bytes());
        // A reply that came before the call ended is handed back.
        assert!(b.write(message(&next.to_le_bytes(), vec![])).is_ok());
        assert!(a.abandon_call(next).is_some());
    }

    #[test]
    fn abandons_calls_without_allocating() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let ids: std::vec::Vec<u32> = (0..5).map(|_| a.begin_call().unwrap()).collect();
        let capacity = {
            let shared = a.shared.lock();
            let queue = &shared.queues[a.side];
            assert!(queue.abandoned.capacity() >= queue.abandoned.len() + queue.calls.len());
            queue.abandoned.capacity()
        };
        for &id in &ids {
            assert!(a.abandon_call(id).is_none());
        }
        assert_eq!(
            a.shared.lock().queues[a.side].abandoned.capacity(),
            capacity
        );
        for &id in &ids {
            assert_eq!(b.write(message(&id.to_le_bytes(), vec![])).ok(), Some(None));
        }
        assert_eq!(read(&a), Err(Error::ShouldWait));
    }

    #[test]
    fn delivers_in_order_to_the_peer_only() {
        let (a, b) = Endpoint::pair(None).unwrap();
        assert_eq!(a.header().signals(), signals::WRITABLE);
        a.write(message(b"one", vec![])).ok().unwrap();
        a.write(message(b"two", vec![])).ok().unwrap();
        assert_eq!(read(&a), Err(Error::ShouldWait));
        assert_eq!(b.header().signals(), signals::WRITABLE | signals::READABLE);
        assert_eq!(read(&b).unwrap().0, b"one");
        assert_eq!(read(&b).unwrap().0, b"two");
        assert_eq!(b.header().signals(), signals::WRITABLE);
        assert_eq!(a.related_koid(), b.header().koid());
    }

    #[test]
    fn keeps_a_message_that_was_not_accepted() {
        let (a, b) = Endpoint::pair(None).unwrap();
        a.write(message(b"x", vec![])).ok().unwrap();
        assert_eq!(
            b.read::<Error>(|_, _| Err(Error::BufferTooSmall)),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(read(&b).unwrap().0, b"x");
    }

    #[test]
    fn fills_up_by_count_and_by_bytes() {
        let (a, b) = Endpoint::pair(None).unwrap();
        for _ in 0..QUEUE_MESSAGES {
            a.write(message(b"", vec![])).ok().unwrap();
        }
        assert_eq!(a.header().signals() & signals::WRITABLE, 0);
        let (e, _) = a.write(message(b"", vec![])).err().unwrap();
        assert_eq!(e, Error::ShouldWait);
        read(&b).unwrap();
        assert_ne!(a.header().signals() & signals::WRITABLE, 0);

        let (a, _b) = Endpoint::pair(None).unwrap();
        let big = vec![0u8; BYTES_MAX];
        for _ in 0..QUEUE_BYTES / BYTES_MAX {
            a.write(message(&big, vec![])).ok().unwrap();
        }
        let (e, _) = a.write(message(b"", vec![])).err().unwrap();
        assert_eq!(e, Error::ShouldWait);
    }

    #[test]
    fn closing_discards_toward_it_and_keeps_toward_the_peer() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let carried: Arc<dyn Object> = Arc::new(Plain(Header::new(ObjectType::Event)));
        let weak = Arc::downgrade(&carried);
        let entry = Entry {
            object: carried,
            rights: 0,
        };
        b.write(message(b"to a", vec![entry])).ok().unwrap();
        a.write(message(b"to b", vec![])).ok().unwrap();
        drop(a);
        assert!(weak.upgrade().is_none());
        assert_eq!(
            b.header().signals(),
            signals::READABLE | signals::PEER_CLOSED
        );
        assert_eq!(read(&b).unwrap().0, b"to b");
        assert_eq!(read(&b), Err(Error::PeerClosed));
        let (e, _) = b.write(message(b"", vec![])).err().unwrap();
        assert_eq!(e, Error::PeerClosed);
    }

    #[test]
    fn tells_the_observer_when_the_peer_goes() {
        struct Count(core::sync::atomic::AtomicU32);
        impl PeerObserver for Count {
            fn peer_closed(&self) {
                self.0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
        let count = Arc::new(Count(Default::default()));
        let (a, b) = Endpoint::pair(None).unwrap();
        let weak: Weak<dyn PeerObserver> = Arc::downgrade(&count) as _;
        a.set_observer(weak);
        assert!(!a.peer_closed());
        drop(b);
        assert!(a.peer_closed());
        assert_eq!(count.0.load(core::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn charges_messages_until_they_are_read() {
        let budget = Budget::root();
        let (a, b) = Endpoint::pair(Some(&budget)).unwrap();
        let m = Message::new(vec![1, 2, 3], Vec::new(), Some(&budget)).unwrap();
        a.write(m).ok().unwrap();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[8..16].try_into().unwrap());
        assert!(used(&budget) >= CHANNEL_COST + 3);
        read(&b).unwrap();
        assert_eq!(used(&budget), CHANNEL_COST);
        drop(a);
        assert_eq!(used(&budget), CHANNEL_COST);
        drop(b);
        assert_eq!(used(&budget), 0);
    }

    #[test]
    fn take_returns_the_message_with_its_charge() {
        let budget = Budget::root();
        let (a, b) = Endpoint::pair(Some(&budget)).unwrap();
        let m = Message::new(vec![1, 2, 3], Vec::new(), Some(&budget)).unwrap();
        a.write(m).ok().unwrap();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[8..16].try_into().unwrap());
        let charged = used(&budget);
        let (len, message) = b.take(|bytes, _| Ok::<_, Error>(bytes.len())).unwrap();
        assert_eq!((len, message.bytes.as_slice()), (3, &[1, 2, 3][..]));
        assert_eq!(b.header().signals() & signals::READABLE, 0);
        assert_eq!(used(&budget), charged);
        drop(message);
        assert_eq!(used(&budget), CHANNEL_COST);
        assert_eq!(read(&b), Err(Error::ShouldWait));
    }
}
