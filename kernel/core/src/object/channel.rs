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

#[derive(Default)]
struct Queue {
    messages: VecDeque<Message>,
    bytes: usize,
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
            })
        };
        let (a, b) = (end(0), end(1));
        shared.lock().ends = [Arc::downgrade(&a), Arc::downgrade(&b)];
        Ok((a, b))
    }

    fn peer_side(&self) -> usize {
        1 - self.side
    }

    /// Queues `message` toward the peer. Fails with `PeerClosed` when the
    /// peer is gone and `ShouldWait` when its queue is full, giving the
    /// message back.
    pub fn write(&self, message: Message) -> Result<(), (Error, Message)> {
        let mut shared = self.shared.lock();
        let peer = shared.ends[self.peer_side()].clone();
        if peer.strong_count() == 0 {
            return Err((Error::PeerClosed, message));
        }
        let queue = &mut shared.queues[self.peer_side()];
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
        Ok(())
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
        accept(&message.bytes, &message.handles)?;
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
        // The handles were installed by `accept`; dropping the message
        // drops only its own references.
        drop(message);
        Ok(())
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
}
