//! Streams: two endpoints with byte queues between them, without message
//! boundaries.

use super::budget::Budget;
use super::{Header, Object, ObjectType, signals};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};

/// The bytes the queue toward each endpoint holds.
pub const QUEUE_BYTES: usize = 65536;
/// The largest write that is appended completely or not at all.
pub const ATOMIC_WRITE: usize = 4096;
/// The kernel memory charged for a stream: its endpoints, the state they
/// share and both queues.
pub const STREAM_COST: u64 =
    (2 * size_of::<StreamEnd>() + size_of::<SpinLock<Shared>>() + 2 * QUEUE_BYTES) as u64;

/// The state both endpoints share. Queue `i` holds the bytes toward
/// endpoint `i`.
struct Shared {
    queues: [VecDeque<u8>; 2],
    ends: [Weak<StreamEnd>; 2],
    charged: Option<Arc<Budget>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Some(budget) = &self.charged {
            budget.release_kernel(STREAM_COST);
        }
    }
}

pub struct StreamEnd {
    header: Header,
    side: usize,
    shared: Arc<SpinLock<Shared>>,
}

impl StreamEnd {
    /// A new stream's two endpoints, both `WRITABLE`, with their queues
    /// allocated and `STREAM_COST` charged to `budget`, if given.
    pub fn pair(budget: Option<&Arc<Budget>>) -> Result<(Arc<StreamEnd>, Arc<StreamEnd>), Error> {
        let mut queues = [VecDeque::new(), VecDeque::new()];
        for q in &mut queues {
            q.try_reserve_exact(QUEUE_BYTES)
                .map_err(|_| Error::NoMemory)?;
        }
        if let Some(budget) = budget {
            budget.charge_kernel(STREAM_COST)?;
        }
        let shared = Arc::new(SpinLock::new(Shared {
            queues,
            ends: [Weak::new(), Weak::new()],
            charged: budget.cloned(),
        }));
        let end = |side| {
            let header = Header::new(ObjectType::Stream);
            header.update_signals(0, signals::WRITABLE);
            Arc::new(StreamEnd {
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

    /// Appends as many of `bytes` as fit to the queue toward the peer, all
    /// or none of a write of at most `ATOMIC_WRITE` bytes, and returns
    /// their number. Fails with `PeerClosed` when the peer is gone and
    /// `ShouldWait` when no byte could be appended.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, Error> {
        let mut shared = self.shared.lock();
        let peer = shared.ends[self.peer_side()].clone();
        if peer.strong_count() == 0 {
            return Err(Error::PeerClosed);
        }
        let queue = &mut shared.queues[self.peer_side()];
        let room = QUEUE_BYTES - queue.len();
        if room == 0 || (bytes.len() <= ATOMIC_WRITE && bytes.len() > room) {
            return Err(Error::ShouldWait);
        }
        let n = bytes.len().min(room);
        queue.extend(&bytes[..n]);
        if queue.len() == QUEUE_BYTES {
            self.header.update_signals(signals::WRITABLE, 0);
        }
        // As for channels, the peer is set READABLE with the lock held,
        // and dropping the upgraded peer waits for the lock.
        let peer = peer.upgrade();
        if let Some(peer) = &peer
            && n > 0
        {
            peer.header.update_signals(0, signals::READABLE);
        }
        drop(shared);
        drop(peer);
        Ok(n)
    }

    /// Copies up to `buf.len()` bytes from the start of the queue toward
    /// this endpoint into `buf`, leaving them queued, and returns their
    /// number. Fails with `PeerClosed` when the queue is empty and the
    /// peer is gone, and `ShouldWait` when it is empty otherwise.
    pub fn peek(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let shared = self.shared.lock();
        self.copy_front(&shared, buf)
    }

    /// Copies up to `buf.len()` bytes from the start of the queue toward
    /// this endpoint into `buf` and removes them, under one lock, and
    /// returns their number. Fails as `peek` does.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let shared = self.shared.lock();
        let n = self.copy_front(&shared, buf)?;
        self.remove_front(shared, n);
        Ok(n)
    }

    fn copy_front(&self, shared: &Shared, buf: &mut [u8]) -> Result<usize, Error> {
        let queue = &shared.queues[self.side];
        if queue.is_empty() {
            return Err(match shared.ends[self.peer_side()].strong_count() {
                0 => Error::PeerClosed,
                _ => Error::ShouldWait,
            });
        }
        let n = buf.len().min(queue.len());
        for (to, from) in buf[..n].iter_mut().zip(queue.iter()) {
            *to = *from;
        }
        Ok(n)
    }

    /// Removes `n` bytes, at most those just copied, from the start of the
    /// queue toward this endpoint.
    fn remove_front(&self, mut shared: crate::sync::Guard<'_, Shared>, n: usize) {
        let peer = shared.ends[self.peer_side()].clone();
        let queue = &mut shared.queues[self.side];
        let n = n.min(queue.len());
        queue.drain(..n);
        if queue.is_empty() {
            self.header.update_signals(signals::READABLE, 0);
        }
        let peer = peer.upgrade();
        if let Some(peer) = &peer
            && n > 0
        {
            peer.header.update_signals(0, signals::WRITABLE);
        }
        drop(shared);
        drop(peer);
    }
}

impl Object for StreamEnd {
    fn header(&self) -> &Header {
        &self.header
    }
}

impl Drop for StreamEnd {
    fn drop(&mut self) {
        let peer = self.shared.lock().ends[self.peer_side()].clone();
        if let Some(peer) = peer.upgrade() {
            peer.header
                .update_signals(signals::WRITABLE, signals::PEER_CLOSED);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn read(end: &StreamEnd, max: usize) -> Result<std::vec::Vec<u8>, Error> {
        let mut buf = vec![0; max];
        let n = end.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    #[test]
    fn peeks_without_removing() {
        let (a, b) = StreamEnd::pair(None).unwrap();
        assert_eq!(a.write(b"abcd"), Ok(4));
        let mut buf = [0; 3];
        assert_eq!(b.peek(&mut buf), Ok(3));
        assert_eq!(&buf, b"abc");
        assert_eq!(read(&b, 8).unwrap(), b"abcd");
        assert_eq!(b.header().signals() & signals::READABLE, 0);
    }

    #[test]
    fn moves_bytes_without_boundaries() {
        let (a, b) = StreamEnd::pair(None).unwrap();
        assert_eq!(a.write(b"abc"), Ok(3));
        assert_eq!(a.write(b"de"), Ok(2));
        assert_eq!(b.header().signals() & signals::READABLE, signals::READABLE);
        assert_eq!(read(&b, 4).unwrap(), b"abcd");
        assert_eq!(read(&b, 4).unwrap(), b"e");
        assert_eq!(b.header().signals() & signals::READABLE, 0);
        assert_eq!(read(&b, 4), Err(Error::ShouldWait));
        assert_eq!(read(&a, 4), Err(Error::ShouldWait));
    }

    #[test]
    fn fills_the_queue_and_keeps_small_writes_whole() {
        let (a, b) = StreamEnd::pair(None).unwrap();
        let big = vec![7u8; QUEUE_BYTES - 100];
        assert_eq!(a.write(&big), Ok(QUEUE_BYTES - 100));
        // A small write that does not fit is refused whole.
        assert_eq!(a.write(&[1; 200]), Err(Error::ShouldWait));
        // A large one takes what fits.
        assert_eq!(a.write(&vec![2; 5000]), Ok(100));
        assert_eq!(a.header().signals() & signals::WRITABLE, 0);
        assert_eq!(a.write(&vec![2; 5000]), Err(Error::ShouldWait));
        assert_eq!(read(&b, 10).unwrap().len(), 10);
        assert_eq!(a.header().signals() & signals::WRITABLE, signals::WRITABLE);
    }

    #[test]
    fn keeps_bytes_after_the_peer_closes() {
        let (a, b) = StreamEnd::pair(None).unwrap();
        a.write(b"last").unwrap();
        drop(a);
        assert_eq!(
            b.header().signals() & signals::PEER_CLOSED,
            signals::PEER_CLOSED
        );
        assert_eq!(b.header().signals() & signals::WRITABLE, 0);
        assert_eq!(b.write(b"x"), Err(Error::PeerClosed));
        assert_eq!(read(&b, 10).unwrap(), b"last");
        assert_eq!(read(&b, 10), Err(Error::PeerClosed));
    }

    #[test]
    fn charges_its_budget() {
        let budget = Budget::root();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[0x08..0x10].try_into().unwrap());
        let (a, b) = StreamEnd::pair(Some(&budget)).unwrap();
        assert_eq!(used(&budget), STREAM_COST);
        drop((a, b));
        assert_eq!(used(&budget), 0);
    }
}
