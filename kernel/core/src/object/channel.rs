//! Channels: two endpoints, each with a queue of messages written to the
//! other.

use super::budget::{Budget, Held};
use super::handles::Entry;
use super::{Header, Object, ObjectType, signals};
use crate::fallible::try_arc;
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::mem;
use core::ops::Deref;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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
    _charge: Option<Held>,
    /// The charge for the places in `TRANSIT` of the endpoints it carries.
    _transit: Option<Held>,
}

impl Message {
    /// The kernel memory charged for a message of `len` bytes and `count`
    /// handles.
    pub fn cost(len: usize, count: usize) -> u64 {
        (size_of::<Message>() + len + count * size_of::<Entry>()) as u64
    }

    /// A message holding `bytes` and `handles`. Its kernel memory is
    /// charged to `budget`, if given.
    pub fn new(
        bytes: Vec<u8>,
        handles: Vec<Entry>,
        budget: Option<&Arc<Budget>>,
    ) -> Result<Message, Error> {
        let cost = Message::cost(bytes.capacity(), handles.capacity());
        let charge = budget.map(|b| b.hold_kernel(cost)).transpose()?;
        Ok(Message::charged(bytes, handles, charge))
    }

    /// A message holding `bytes` and `handles`, whose kernel memory
    /// `charge` covers.
    pub fn charged(bytes: Vec<u8>, handles: Vec<Entry>, charge: Option<Held>) -> Message {
        Message {
            bytes,
            handles,
            _charge: charge,
            _transit: None,
        }
    }

    /// Charges `TRANSIT_COST` to `budget` for each endpoint the message
    /// carries, until it is dropped.
    pub fn charge_transit(&mut self, budget: &Arc<Budget>) -> Result<(), Error> {
        let cost = TRANSIT_COST * endpoints_in(&self.handles) as u64;
        if cost > 0 {
            self._transit = Some(budget.hold_kernel(cost)?);
        }
        Ok(())
    }
}

impl Drop for Message {
    /// Endpoints that a queued message carried may be left reachable only
    /// through other queued messages, so the collector is told.
    fn drop(&mut self) {
        for entry in self.handles.drain(..) {
            if let Some(core) = queued_core(&entry) {
                core.queued.fetch_sub(1, Ordering::Relaxed);
                changed();
                if core.outer.lock().strong_count() == 0 {
                    need_collection();
                }
            }
        }
    }
}

/// The endpoint state that `entry` refers to when it is queued in a
/// message, where endpoints are held by their `Core`.
fn queued_core(entry: &Entry) -> Option<&Core> {
    (&*entry.object as &dyn Any).downcast_ref::<Core>()
}

/// Turns the endpoints among `entries`, which are about to be queued,
/// into references to their `Core`, so that a queued message does not
/// keep an `Endpoint` alive. Each must be in `TRANSIT` already.
fn to_queued(entries: &mut [Entry]) {
    for entry in entries {
        let Some(core) = (&*entry.object as &dyn Any)
            .downcast_ref::<Endpoint>()
            .map(|e| e.core.clone())
        else {
            continue;
        };
        core.queued.fetch_add(1, Ordering::Relaxed);
        entry.object = core;
        changed();
    }
}

/// Turns the endpoints among `entries`, about to leave a queue, back into
/// `Endpoint`s. Fails with `NoMemory`, changing nothing, when one cannot
/// be made.
fn to_handles(entries: &mut [Entry]) -> Result<(), Error> {
    for i in 0..entries.len() {
        if queued_core(&entries[i]).is_none() {
            continue;
        }
        let core = super::downcast::<Core>(entries[i].object.clone()).expect("a queued endpoint");
        match core.endpoint() {
            Ok(endpoint) => {
                core.queued.fetch_sub(1, Ordering::Relaxed);
                entries[i].object = endpoint;
                changed();
            }
            Err(e) => {
                to_queued(&mut entries[..i]);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Adds the endpoints among `entries` that are not yet in `TRANSIT` to
/// it. Fails with `NoMemory`, adding none, when there is no room.
fn register(entries: &[Entry]) -> Result<(), Error> {
    let cores = || {
        entries.iter().filter_map(|e| {
            (&*e.object as &dyn Any)
                .downcast_ref::<Endpoint>()
                .map(|e| &e.core)
        })
    };
    let new = cores()
        .filter(|c| !c.registered.load(Ordering::Relaxed))
        .count();
    if new == 0 {
        return Ok(());
    }
    let mut transit = TRANSIT.lock();
    transit.try_reserve(new).map_err(|_| Error::NoMemory)?;
    for core in cores() {
        if !core.registered.swap(true, Ordering::Relaxed) {
            transit.push(Arc::downgrade(core));
        }
    }
    changed();
    Ok(())
}

/// The kernel memory charged with a message for each endpoint it carries,
/// for its place in `TRANSIT`.
pub const TRANSIT_COST: u64 = size_of::<Weak<Core>>() as u64;

/// The number of endpoints among `entries`.
pub fn endpoints_in(entries: &[Entry]) -> usize {
    entries
        .iter()
        .filter(|e| (&*e.object as &dyn Any).is::<Endpoint>())
        .count()
}

/// The bit that marks transaction IDs of `channel_call`.
pub const CALL_ID: u32 = 1 << 31;

/// An outstanding `channel_call`, with its reply once it arrives.
struct Call {
    id: u32,
    reply: Option<Message>,
    /// The charge for the call, kept while its ID is recorded as
    /// abandoned.
    charge: Option<Held>,
}

/// The kernel memory charged for a call: its entry, its entry in the
/// abandoned calls, and those entries' share of storage.
pub const CALL_COST: u64 = 2 * (size_of::<Call>() + size_of::<(u32, Option<Held>)>()) as u64;

#[derive(Default)]
struct Queue {
    messages: VecDeque<Message>,
    bytes: usize,
    /// The calls made through the endpoint the queue leads to.
    calls: Vec<Call>,
    /// The IDs of calls that ended without their reply, whose late
    /// replies are discarded, with their charges.
    abandoned: Vec<(u32, Option<Held>)>,
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
    ends: [Weak<Core>; 2],
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

/// The kernel memory charged for a channel: its endpoints, each with the
/// one `Endpoint` that refers to it at a time, and the state they share.
pub const CHANNEL_COST: u64 =
    (2 * (size_of::<Core>() + size_of::<Endpoint>()) + size_of::<SpinLock<Shared>>()) as u64;

/// A channel endpoint as handles and the kernel refer to it. Messages
/// queued toward an endpoint refer to the endpoints they carry by their
/// `Core` instead, so that the collector can tell which endpoints only
/// queued messages keep alive.
pub struct Endpoint {
    core: Arc<Core>,
}

impl Deref for Endpoint {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if self.core.queued.load(Ordering::Relaxed) > 0 {
            changed();
            need_collection();
        }
    }
}

/// The state of a channel endpoint, alive while an `Endpoint` or a
/// queued message refers to it.
pub struct Core {
    header: Header,
    side: usize,
    shared: Arc<SpinLock<Shared>>,
    /// Told when the peer is destroyed.
    observer: SpinLock<Option<Weak<dyn PeerObserver>>>,
    /// The `Endpoint` that refers to this, if one does.
    outer: SpinLock<Weak<Endpoint>>,
    /// The number of queued messages that carry this endpoint.
    queued: AtomicUsize,
    /// Whether this is in `TRANSIT`.
    registered: AtomicBool,
    /// The collector's marks: the collection whose set of examined
    /// endpoints this is in,
    /// how many of the references counted come from that set, whether it
    /// is reachable, and the next endpoint to examine.
    member: AtomicU64,
    internal: AtomicUsize,
    live: AtomicBool,
    next: SpinLock<Option<Arc<Core>>>,
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
        let (a, b) = Core::pair(budget)?;
        Ok((a.endpoint_of()?, b.endpoint_of()?))
    }
}

impl Core {
    fn pair(budget: Option<&Arc<Budget>>) -> Result<(Arc<Core>, Arc<Core>), Error> {
        if let Some(budget) = budget {
            budget.charge_kernel(CHANNEL_COST)?;
        }
        // Shared releases the charge when it goes, also on failure here.
        let shared = try_arc(SpinLock::new(Shared {
            queues: [Queue::default(), Queue::default()],
            ends: [Weak::new(), Weak::new()],
            charged: budget.cloned(),
        }))?;
        let end = |side| {
            let header = Header::new(ObjectType::Channel);
            header.update_signals(0, signals::WRITABLE);
            try_arc(Core {
                header,
                side,
                shared: shared.clone(),
                observer: SpinLock::new(None),
                outer: SpinLock::new(Weak::new()),
                queued: AtomicUsize::new(0),
                registered: AtomicBool::new(false),
                member: AtomicU64::new(0),
                internal: AtomicUsize::new(0),
                live: AtomicBool::new(false),
                next: SpinLock::new(None),
            })
        };
        let (a, b) = (end(0)?, end(1)?);
        shared.lock().ends = [Arc::downgrade(&a), Arc::downgrade(&b)];
        Ok((a, b))
    }

    /// The `Endpoint` that refers to this, made if none does.
    fn endpoint(self: &Arc<Core>) -> Result<Arc<dyn Object>, Error> {
        let endpoint: Arc<dyn Object> = self.endpoint_of()?;
        Ok(endpoint)
    }

    fn endpoint_of(self: &Arc<Core>) -> Result<Arc<Endpoint>, Error> {
        let mut outer = self.outer.lock();
        if let Some(endpoint) = outer.upgrade() {
            return Ok(endpoint);
        }
        let endpoint = try_arc(Endpoint { core: self.clone() })?;
        *outer = Arc::downgrade(&endpoint);
        changed();
        Ok(endpoint)
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
        // Room in `TRANSIT` for the endpoints the message carries is
        // made first, as only that can fail once the message is placed.
        let mut message = message;
        if let Err(e) = register(&message.handles) {
            return Err((e, message));
        }
        if let Some(id) = id {
            if let Some(call) = queue
                .calls
                .iter_mut()
                .find(|c| c.id == id && c.reply.is_none())
            {
                to_queued(&mut message.handles);
                call.reply = Some(message);
                drop(shared);
                let koid = peer.upgrade().map_or(0, |p| p.header.koid());
                return Ok(Some((koid, id)));
            }
            if let Some(i) = queue.abandoned.iter().position(|a| a.0 == id) {
                let abandoned = queue.abandoned.swap_remove(i);
                // The message and its handles are dropped with no lock
                // held, as that may destroy objects.
                drop(shared);
                drop(message);
                drop(abandoned);
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
        to_queued(&mut message.handles);
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

    /// Removes the first message that this endpoint wrote, still queued
    /// for the peer, for which `matches` holds, and returns it so that
    /// the caller drops it with no lock held.
    pub fn unwrite(&self, matches: impl Fn(&Message) -> bool) -> Option<Message> {
        let mut shared = self.shared.lock();
        let peer = shared.ends[self.peer_side()].clone();
        let queue = &mut shared.queues[self.peer_side()];
        let i = queue.messages.iter().position(matches)?;
        let message = queue.messages.remove(i).expect("found above");
        queue.bytes -= message.bytes.len();
        if !queue.full() {
            self.header.update_signals(0, signals::WRITABLE);
        }
        let peer = if queue.messages.is_empty() {
            peer.upgrade()
        } else {
            None
        };
        if let Some(peer) = &peer {
            peer.header.update_signals(signals::READABLE, 0);
        }
        drop(shared);
        drop(peer);
        Some(message)
    }

    /// Starts a call through this endpoint and returns its transaction
    /// ID: one with `CALL_ID` set that no outstanding call and no call
    /// ended without its reply uses. `CALL_COST` is charged to `budget`,
    /// if given, until the call ends and its reply is read or discarded.
    pub fn begin_call(&self, budget: Option<&Arc<Budget>>) -> Result<u32, Error> {
        let charge = budget.map(|b| b.hold_kernel(CALL_COST)).transpose()?;
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
            let used =
                queue.calls.iter().any(|c| c.id == id) || queue.abandoned.iter().any(|a| a.0 == id);
            if !used {
                break id;
            }
        };
        queue.calls.push(Call {
            id,
            reply: None,
            charge,
        });
        Ok(id)
    }

    /// Ends call `id` and returns its reply, if it has arrived; otherwise
    /// the call stays outstanding and `None` is returned. Fails with
    /// `NoMemory`, leaving the reply in place, when the endpoints it
    /// carries cannot be given back as handles.
    pub fn take_reply(&self, id: u32) -> Result<Option<Message>, Error> {
        let mut shared = self.shared.lock();
        let calls = &mut shared.queues[self.side].calls;
        let Some(i) = calls.iter().position(|c| c.id == id && c.reply.is_some()) else {
            return Ok(None);
        };
        to_handles(&mut calls[i].reply.as_mut().expect("checked above").handles)?;
        Ok(calls.swap_remove(i).reply)
    }

    /// Ends call `id` without its reply: a reply arriving later is
    /// discarded. Returns a reply that arrived meanwhile, which the
    /// caller drops with no lock held.
    pub fn abandon_call(&self, id: u32) -> Option<Message> {
        let mut shared = self.shared.lock();
        let queue = &mut shared.queues[self.side];
        let i = queue.calls.iter().position(|c| c.id == id)?;
        let Call { reply, charge, .. } = queue.calls.swap_remove(i);
        if reply.is_none() {
            debug_assert!(queue.abandoned.len() < queue.abandoned.capacity());
            queue.abandoned.push((id, charge));
        }
        reply
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
        let Some(message) = queue.messages.front_mut() else {
            return Err(match peer.strong_count() {
                0 => Error::PeerClosed,
                _ => Error::ShouldWait,
            }
            .into());
        };
        to_handles(&mut message.handles)?;
        let accepted = match accept(&message.bytes, &message.handles) {
            Ok(accepted) => accepted,
            Err(e) => {
                to_queued(&mut message.handles);
                return Err(e);
            }
        };
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
        &self.core.header
    }

    fn related_koid(&self) -> u64 {
        self.core.related_koid()
    }
}

impl Object for Core {
    fn header(&self) -> &Header {
        &self.header
    }

    fn related_koid(&self) -> u64 {
        let peer = self.shared.lock().ends[self.peer_side()].clone();
        peer.upgrade().map_or(0, |peer| peer.header.koid())
    }
}

impl Drop for Core {
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
        // further objects, so this happens with no lock held, accounted
        // for one message at a time.
        let Queue {
            messages, calls, ..
        } = discarded;
        let replies = calls.into_iter().filter_map(|c| c.reply);
        for message in messages.into_iter().chain(replies) {
            let count = 1 + message.handles.len();
            drop(message);
            crate::sync::destroyed(count);
        }
    }
}

/// Every endpoint that a queued message may carry. The collector removes
/// those that no queued message carries any more.
static TRANSIT: SpinLock<Vec<Weak<Core>>> = SpinLock::new(Vec::new());
/// Counts changes to which endpoints queued messages carry and to the
/// `Endpoint`s that refer to them, so that a collection can tell whether
/// what it counted still holds.
static CHANGES: AtomicU64 = AtomicU64::new(0);
/// Numbers collections, so that marks of an earlier one do not count.
static COLLECTION: AtomicU64 = AtomicU64::new(0);
/// Set when an endpoint may have become reachable only through queued
/// messages.
static NEEDED: AtomicBool = AtomicBool::new(false);
/// Set while a collection runs.
static COLLECTING: AtomicBool = AtomicBool::new(false);

fn changed() {
    CHANGES.fetch_add(1, Ordering::AcqRel);
}

fn need_collection() {
    NEEDED.store(true, Ordering::Release);
}

/// Destroys the endpoints that only queued messages toward other such
/// endpoints keep alive, while a collection is needed, unless another
/// thread is collecting already. It must be called with no lock held:
/// it counts each endpoint and message it examines with
/// `sync::destroyed`, which may let other threads run.
pub fn collect_if_needed() {
    if !NEEDED.load(Ordering::Acquire) || COLLECTING.swap(true, Ordering::AcqRel) {
        return;
    }
    while NEEDED.swap(false, Ordering::AcqRel) {
        // A change while it counted makes the collection start again.
        while !collect() {}
    }
    COLLECTING.store(false, Ordering::Release);
}

/// The endpoint at `index` in `TRANSIT` or a later one, moving `index` to
/// it. With `prune`, endpoints that are gone or that no queued message
/// carries are removed on the way.
fn transit_at(index: &mut usize, prune: bool) -> Option<Arc<Core>> {
    loop {
        let mut transit = TRANSIT.lock();
        let core = transit.get(*index)?.upgrade();
        match core {
            Some(core) if !prune || core.queued.load(Ordering::Relaxed) > 0 => return Some(core),
            _ => {
                transit.swap_remove(*index);
                drop(transit);
                if let Some(core) = core {
                    core.registered.store(false, Ordering::Relaxed);
                    // The last reference may go here, with no lock held.
                    drop(core);
                }
            }
        }
    }
}

/// Calls `f` with each endpoint in `TRANSIT` that this collection
/// examines.
fn members(collection: u64, mut f: impl FnMut(&Arc<Core>)) {
    let mut i = 0;
    while let Some(core) = transit_at(&mut i, false) {
        if core.member.load(Ordering::Relaxed) == collection {
            f(&core);
        }
        drop(core);
        crate::sync::destroyed(1);
        i += 1;
    }
}

/// Calls `f` with each endpoint that the messages queued toward `core`
/// carry, and returns the number of messages.
fn carried(core: &Core, mut f: impl FnMut(&Entry, &Core)) -> usize {
    let shared = core.shared.lock();
    let queue = &shared.queues[core.side];
    let replies = queue.calls.iter().filter_map(|c| c.reply.as_ref());
    let mut count = 0;
    for message in queue.messages.iter().chain(replies) {
        count += 1;
        for entry in &message.handles {
            if let Some(x) = queued_core(entry) {
                f(entry, x);
            }
        }
    }
    count
}

/// One collection. Returns false, destroying nothing, when what it
/// counted changed meanwhile.
fn collect() -> bool {
    let start = CHANGES.load(Ordering::Acquire);
    let collection = COLLECTION.fetch_add(1, Ordering::Relaxed) + 1;
    // The examined set: every endpoint that a queued message carries.
    let mut i = 0;
    while let Some(core) = transit_at(&mut i, true) {
        core.member.store(collection, Ordering::Relaxed);
        core.internal.store(0, Ordering::Relaxed);
        core.live.store(false, Ordering::Relaxed);
        drop(core);
        crate::sync::destroyed(1);
        i += 1;
    }
    // The references to each that come from messages queued toward the
    // examined endpoints.
    members(collection, |core| {
        let count = carried(core, |_, x| {
            if x.member.load(Ordering::Relaxed) == collection {
                x.internal.fetch_add(1, Ordering::Relaxed);
            }
        });
        crate::sync::destroyed(count);
    });
    // Any other reference makes an endpoint reachable, and with it the
    // endpoints that messages queued toward it carry. The reference that
    // `members` holds is left out.
    let mut reachable: Option<Arc<Core>> = None;
    members(collection, |core| {
        let others = Arc::strong_count(core) - 1;
        if others > core.internal.load(Ordering::Relaxed) {
            core.live.store(true, Ordering::Relaxed);
            *core.next.lock() = reachable.take();
            reachable = Some(core.clone());
        }
    });
    while let Some(core) = reachable.take() {
        reachable = core.next.lock().take();
        let count = carried(&core, |entry, x| {
            if x.member.load(Ordering::Relaxed) == collection
                && !x.live.swap(true, Ordering::Relaxed)
            {
                let x = super::downcast::<Core>(entry.object.clone()).expect("a queued endpoint");
                *x.next.lock() = reachable.take();
                reachable = Some(x);
            }
        });
        drop(core);
        crate::sync::destroyed(1 + count);
    }
    if CHANGES.load(Ordering::Acquire) != start {
        return false;
    }
    // Nothing reaches the rest, so nothing can change them: their queues
    // are discarded, which destroys them once the last message carrying
    // each is gone.
    members(collection, |core| {
        if core.live.load(Ordering::Relaxed) {
            return;
        }
        let discarded = mem::take(&mut core.shared.lock().queues[core.side]);
        let Queue {
            messages, calls, ..
        } = discarded;
        let replies = calls.into_iter().filter_map(|c| c.reply);
        for message in messages.into_iter().chain(replies) {
            let count = 1 + message.handles.len();
            drop(message);
            crate::sync::destroyed(count);
        }
    });
    true
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

    /// Serializes the tests that collect, as a collection assumes that
    /// no other one runs.
    static COLLECTOR: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn collect_now() {
        let _one = COLLECTOR.lock().unwrap_or_else(|e| e.into_inner());
        while !collect() {}
    }

    fn carrying(end: &Arc<Endpoint>) -> Message {
        let entry = Entry {
            object: end.clone(),
            rights: 0,
        };
        message(b"x", vec![entry])
    }

    fn used(budget: &Arc<Budget>) -> u64 {
        u64::from_le_bytes(budget.info()[0x08..0x10].try_into().unwrap())
    }

    #[test]
    fn destroys_endpoints_that_carry_each_other() {
        let budget = Budget::child(&Budget::root(), u64::MAX, u64::MAX, None).unwrap();
        let (a, b) = Endpoint::pair(Some(&budget)).unwrap();
        let (c, d) = Endpoint::pair(Some(&budget)).unwrap();
        // b's queue carries d and d's queue carries b.
        a.write(carrying(&d)).ok().unwrap();
        c.write(carrying(&b)).ok().unwrap();
        let (b_core, d_core) = (Arc::downgrade(&b.core), Arc::downgrade(&d.core));
        drop((a, b, c, d));
        assert!(b_core.upgrade().is_some() && d_core.upgrade().is_some());
        assert!(NEEDED.load(Ordering::Acquire));
        collect_now();
        assert!(b_core.upgrade().is_none() && d_core.upgrade().is_none());
        assert_eq!(used(&budget), 0);
    }

    #[test]
    fn keeps_endpoints_queued_toward_reachable_ones() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let (c, d) = Endpoint::pair(None).unwrap();
        let (e, f) = Endpoint::pair(None).unwrap();
        // b, reachable, carries c; c carries e, which carries c.
        a.write(carrying(&c)).ok().unwrap();
        d.write(carrying(&e)).ok().unwrap();
        f.write(carrying(&c)).ok().unwrap();
        drop((c, e));
        collect_now();
        let mut got = None;
        b.read::<Error>(|_, h| {
            got = super::super::downcast::<Endpoint>(h[0].object.clone());
            Ok(())
        })
        .unwrap();
        let c = got.expect("an endpoint is given back");
        assert_eq!(c.header().signals() & signals::PEER_CLOSED, 0);
        assert_eq!(read(&c).unwrap().0, b"x");
        drop((a, b, d, f));
    }

    #[test]
    fn charges_endpoints_in_messages() {
        let budget = Budget::child(&Budget::root(), u64::MAX, u64::MAX, None).unwrap();
        let (a, b) = Endpoint::pair(None).unwrap();
        let mut m = carrying(&b);
        m.charge_transit(&budget).unwrap();
        assert_eq!(used(&budget), TRANSIT_COST);
        drop(m);
        assert_eq!(used(&budget), 0);
        drop((a, b));
    }

    #[test]
    fn removes_a_written_message() {
        let (a, b) = Endpoint::pair(None).unwrap();
        for byte in [1, 2, 1] {
            a.write(message(&[byte], vec![])).ok().unwrap();
        }
        assert!(a.unwrite(|m| m.bytes == [3]).is_none());
        // Messages written toward `a` are not this endpoint's to remove.
        b.write(message(&[2], vec![])).ok().unwrap();
        assert_eq!(a.unwrite(|m| m.bytes == [2]).unwrap().bytes, [2]);
        assert!(a.unwrite(|m| m.bytes == [2]).is_none());
        assert_eq!(read(&b).unwrap().0, [1]);
        assert_eq!(a.unwrite(|m| m.bytes == [1]).unwrap().bytes, [1]);
        assert_eq!(b.header().signals() & signals::READABLE, 0);
        assert_ne!(a.header().signals() & signals::WRITABLE, 0);
        assert_eq!(read(&a).unwrap().0, [2]);
    }

    #[test]
    fn delivers_replies_to_their_calls() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let (one, two) = (a.begin_call(None).unwrap(), a.begin_call(None).unwrap());
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
        assert!(a.take_reply(one).unwrap().is_none());
        assert_eq!(a.take_reply(two).unwrap().unwrap().bytes, reply);
        // Other IDs with the bit set are ordinary messages.
        let other = (CALL_ID | 77).to_le_bytes();
        assert_eq!(b.write(message(&other, vec![])).ok(), Some(None));
        assert_eq!(read(&a).unwrap().0, other);
    }

    #[test]
    fn charges_calls_until_their_replies_go() {
        use crate::object::budget::UNLIMITED;
        let root = Budget::root();
        let small = Budget::child(&root, CALL_COST, UNLIMITED, None).unwrap();
        let used = |b: &Budget| u64::from_le_bytes(b.info()[8..16].try_into().unwrap());
        let (a, b) = Endpoint::pair(None).unwrap();
        let id = a.begin_call(Some(&small)).unwrap();
        assert_eq!(a.begin_call(Some(&small)), Err(Error::NoResources));
        // An abandoned call stays charged until its late reply comes.
        assert!(a.abandon_call(id).is_none());
        assert_eq!(used(&small), CALL_COST);
        assert_eq!(b.write(message(&id.to_le_bytes(), vec![])).ok(), Some(None));
        assert_eq!(used(&small), 0);
        let id = a.begin_call(Some(&small)).unwrap();
        b.write(message(&id.to_le_bytes(), vec![])).ok().unwrap();
        assert!(a.take_reply(id).unwrap().is_some());
        assert_eq!((used(&small), used(&root)), (0, 0));
    }

    #[test]
    fn discards_replies_to_abandoned_calls() {
        let (a, b) = Endpoint::pair(None).unwrap();
        let id = a.begin_call(None).unwrap();
        assert!(a.abandon_call(id).is_none());
        let next = a.begin_call(None).unwrap();
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
        let ids: std::vec::Vec<u32> = (0..5).map(|_| a.begin_call(None).unwrap()).collect();
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
