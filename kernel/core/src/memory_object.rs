//! Anonymous memory objects: their frames and the copies to and from them.

use crate::arch::CachePolicy as Policy;
use crate::aspace::{self, Access, AddressSpace, Fault};
use crate::frames::FRAMES;
use crate::port::Port;
use crate::publish::{ITEM_STEP, Work};
use crate::share::{Released, Write};

use crate::nodes::{self, Map};
use crate::object::budget::{Budget, Scratch};
use crate::object::memory::{
    Frame, INFO_MEMORY_SIZE, KIND_ANONYMOUS, KIND_CONTIGUOUS, KIND_PAGED, KIND_PHYSICAL,
    KIND_PINNED, PAGE_ENTRY_COST, PAGE_SIZE, Pages, RESIZABLE, Ranges, SEAL_GROW, SEAL_SEAL,
    SEAL_SHRINK, SEAL_WRITE, SEALS, STABLE, TRACK_DIRTY,
};
use crate::object::{Header, Object, ObjectType};
use crate::paging::{Absent, Paging};
use crate::regions::{PERM_EXECUTE, PERM_WRITE, RECORD_ROOM, Record};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The largest memory object: the size of the user address range.
fn max_size() -> u64 {
    let (start, end) = aspace::user_range();
    end - start
}

/// The most bytes copied while the pages are locked. Copies to and from
/// user memory happen in between, without the lock, as they can fault.
const CHUNK: usize = 256;

pub struct MemoryObject {
    header: Header,
    /// The budget that the object and its frames are charged to: that of
    /// the creating process, or the one it named, or `None` for an object
    /// the kernel made at boot, which charges nothing. A paged object
    /// always has one.
    budget: Option<Arc<Budget>>,
    /// Kernel memory charged to the budget for the object itself.
    charged: u64,
    pages: SpinLock<Pages>,
    /// For a physical object, the start of its range and the cache policy
    /// of its mappings. Its pages then never hold frames.
    physical: Option<(u64, Policy)>,
    /// The regions that map the object, by the ID of their record and
    /// their start, with their address spaces, so that pages can be
    /// unmapped when the object changes. A region's `Mapping` keeps its
    /// entry, which its `REGION_COST` covers. Locked after the regions.
    mappings: SpinLock<Map<(u64, u64), Weak<AddressSpace>>>,
    /// The number of regions that map the object writable.
    writable: AtomicU64,
    /// The page ranges, first and exclusive end, that were given frames
    /// in place of the shared zero page, or frames of their own in place
    /// of shared ones, and whose old translations are still being removed,
    /// with the old frame to end the use of afterwards. Nothing may write
    /// to their frames or map them until then. Locked after the pages.
    changing: SpinLock<Vec<Changing>>,
    /// The seals of `mem_seal`, never removed. Checks and changes are
    /// serialized because system calls run one at a time on the one
    /// processor.
    seals: AtomicU32,
    /// The `pager_supply` calls under way that move frames out of the
    /// object or into it, counted by `Supplying`. A `WRITE` seal and a
    /// revocation wait until none is left. Raised with the pages locked,
    /// right after the check that the seal or the revocation would fail.
    supplies: AtomicU32,
    /// For a paged object, what its pager needs.
    paged: Option<Paged>,
    /// For a contiguous or pinned object, which all its frames were given
    /// at creation and keep until it is destroyed, its kind.
    fixed: Option<Fixed>,
}

/// The kinds of objects whose frames are allocated at creation and never
/// change: contiguous, with the address of the first frame, and pinned.
#[derive(Clone, Copy)]
pub enum Fixed {
    Contiguous(u64),
    Pinned,
}

/// The pages of a clone that are still to come from a paged object, the
/// origin, which the clone keeps alive and which stays in use for as long
/// as one is left: page `i` of the clone is page `base + i` of the origin.
/// A pending page has no frame.
pub(crate) struct Pending {
    origin: Arc<MemoryObject>,
    base: u64,
    ranges: Ranges,
    /// The budget that the ranges' storage is charged to, and how many
    /// ranges were charged.
    budget: Option<Arc<Budget>>,
    charged: u64,
}

impl Pending {
    /// The pending pages `ranges` of a clone charged to `budget`, which
    /// has paid for them already. The origin is in use from now on.
    fn new(
        origin: Arc<MemoryObject>,
        base: u64,
        ranges: Ranges,
        budget: Option<Arc<Budget>>,
    ) -> Pending {
        origin.use_begin();
        let charged = ranges.len() as u64;
        Pending {
            origin,
            base,
            ranges,
            budget,
            charged,
        }
    }

    /// Takes the pages `first` to `end`, exclusive, out of the pending
    /// ones. Fails, changing nothing, when that needs one more range and
    /// there is no room or no budget for it.
    fn cut(&mut self, first: u64, end: u64) -> Result<(), Error> {
        if self.ranges.splits_inside(first, end) {
            self.ranges.reserve(1)?;
            if let Some(budget) = &self.budget {
                budget.charge_kernel(PAGE_ENTRY_COST)?;
            }
            self.charged += 1;
        }
        self.ranges.cut(first, end)
    }
}

/// What `resolve_pending` took for a page: a frame that the origin and
/// this clone use, or a copy of a lent one.
enum Taken {
    Nothing,
    Shared(u64),
    Copied(u64),
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.release_kernel(self.charged * PAGE_ENTRY_COST);
        }
        self.origin.use_end();
    }
}

/// The part of a paged memory object that its pager drives.
pub struct Paged {
    /// The object ID of the pager.
    pager: u64,
    port: Weak<Port>,
    key: u64,
    /// Locked after the pages.
    state: SpinLock<Paging>,
    /// Set with the state's revocation, for checks that hold locks that
    /// come before the state's, such as an address space's regions.
    revoked: AtomicBool,
    /// The frames that the object has from elsewhere, by frame address:
    /// supplied or lent ones.
    charges: SpinLock<Map<u64, Charge>>,
    /// The byte ranges of the outstanding writebacks, which their ends
    /// must name exactly. Locked after the pages.
    writebacks: SpinLock<Vec<(u64, u64)>>,
    /// How many uses the object has: handles without the right `MANAGE`,
    /// in a table or carried by a queued message, and mappings. The
    /// object's `UNREFERENCED` signal is set exactly while it is 0.
    uses: SpinLock<u64>,
}

/// Pages marked as changing, with the frame that a copy-on-write left,
/// whose use by the slot ends once the old translations are gone.
struct Changing {
    first: u64,
    end: u64,
    old: Option<Frame>,
}

/// A frame of a paged object from elsewhere.
enum Charge {
    /// Supplied to it, and charged to the budget it came with: `None` for
    /// a frame of an object without a budget, which stays uncharged.
    Supplied(Option<Arc<Budget>>),
    /// Lent to it as many times, by an object that keeps the frame and
    /// its charge (`crate::share`).
    Lent(u64),
}

/// What `free` gives back for a frame that leaves an object's pages.
enum Give {
    /// Nothing: the frame stays with the pages that still use it.
    Nothing,
    /// The frame, with its page and the slot's entry, which the object
    /// charged to its own budget.
    Own,
    /// The slot's entry only, charged to the budget given (`None` for
    /// none), as the frame stays with pages that use it.
    Entry(Option<Arc<Budget>>),
    /// The frame, with its page and entry, which the budget it was
    /// supplied with was charged; `None` for none.
    Supplied(Option<Arc<Budget>>),
    /// The frame, as the last use of it ended, with its page charged to
    /// `page`, and the slot's entry, if it had one, charged to `entry`.
    Last {
        page: Option<Arc<Budget>>,
        entry: Option<Option<Arc<Budget>>>,
    },
}

impl Give {
    /// Whether the frame goes back to the allocator.
    fn frees(&self) -> bool {
        matches!(self, Give::Own | Give::Supplied(_) | Give::Last { .. })
    }
}

/// The ops of `pager_op`.
pub const OP_FAIL: u32 = 1;
pub const OP_EVICT: u32 = 2;
pub const OP_WRITEBACK_BEGIN: u32 = 3;
pub const OP_WRITEBACK_END: u32 = 4;
pub const OP_WRITEBACK_ABORT: u32 = 5;
pub const OP_DISCARD: u32 = 6;

/// The `PAGER` packet type and its `READ` command.
const PACKET_PAGER: u32 = 3;
const PAGER_READ: u32 = 1;

/// The bytes of the frame at `phys`.
///
/// # Safety
///
/// The frame must belong to a memory object whose pages are locked, and
/// nothing else may reference its bytes while the result is used.
unsafe fn frame_bytes<'a>(phys: u64) -> &'a mut [u8; PAGE_SIZE as usize] {
    // SAFETY: the direct map covers every frame, and the caller rules out
    // other references.
    unsafe { &mut *(crate::arch::frame_address(phys) as *mut [u8; PAGE_SIZE as usize]) }
}

/// The frame that pages of anonymous objects read before they are
/// written map. Nothing ever writes to it.
static ZERO_FRAME: SpinLock<Option<u64>> = SpinLock::new(crate::lock_order::ZERO_FRAME, None);

/// The shared zero page, allocated on first use.
fn zero_frame() -> Option<u64> {
    let mut zero = ZERO_FRAME.lock();
    if zero.is_none() {
        let phys = crate::frames::alloc_frame()?;
        // SAFETY: the frame was just allocated, so nothing references it.
        unsafe { frame_bytes(phys) }.fill(0);
        *zero = Some(phys);
    }
    *zero
}

/// The page index and the offset inside the page of byte `at`, and the
/// length of the chunk from there, which ends at the page's end, after
/// `CHUNK` bytes or after `left` bytes.
fn chunk(at: u64, left: u64) -> (u64, usize, usize) {
    let inside = (at % PAGE_SIZE) as usize;
    let len = (PAGE_SIZE as usize - inside).min(CHUNK);
    (at / PAGE_SIZE, inside, len.min(left as usize))
}

impl MemoryObject {
    /// An anonymous memory object of `size` bytes created with `options`,
    /// charged to `budget` if given.
    pub fn new(
        budget: Option<&Arc<Budget>>,
        size: u64,
        options: u32,
    ) -> Result<Arc<MemoryObject>, Error> {
        let pages = Pages::new(size, options, max_size())?;
        let charged = match budget {
            Some(budget) => {
                let size = size_of::<MemoryObject>() as u64;
                budget.charge_kernel(size)?;
                size
            }
            None => 0,
        };
        // The object releases the charge when it goes, also on failure.
        crate::fallible::try_arc(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.cloned(),
            charged,
            pages: SpinLock::new(crate::lock_order::MEMORY_PAGES, pages),
            physical: None,
            mappings: SpinLock::new(crate::lock_order::MEMORY_MAPPINGS, nodes::map()),
            writable: AtomicU64::new(0),
            changing: SpinLock::new(crate::lock_order::MEMORY_CHANGING, Vec::new()),
            seals: AtomicU32::new(0),
            supplies: AtomicU32::new(0),
            paged: None,
            fixed: None,
        })
    }

    /// A new anonymous object of `len` bytes, created with `options` and
    /// charged to `budget`, whose bytes are those of the `len` bytes from
    /// `offset` of this one, which are page aligned. Writes to either
    /// object are not seen in the other. Fails with `NotSupported` for a
    /// physical or fixed object and with `OutOfRange` unless the range
    /// lies inside the object.
    ///
    /// An anonymous object's pages are not copied: the clone uses the
    /// frames of the pages that have one, which the frames' owner and the
    /// clone give a copy to when written, and the pages that have no frame
    /// read as zero in the clone. The pages that a paged object uses as
    /// lent frames, and those of a paged object, are copied at once.
    pub fn clone_range<F: FnMut()>(
        self: &Arc<Self>,
        offset: u64,
        len: u64,
        options: u32,
        budget: &Arc<Budget>,
        work: &mut Work<F>,
    ) -> Result<Arc<MemoryObject>, Error> {
        if self.is_physical() || self.is_fixed() {
            return Err(Error::NotSupported);
        }
        self.check_range(offset, len)?;
        let clone = MemoryObject::new(Some(budget), len, options)?;
        // The pages that the clone takes from a paged origin when it
        // first uses them: those that a paged source has not got, and
        // those that an anonymous source takes from its own origin.
        let origin = match &self.paged {
            Some(_) => Some((self.clone(), offset / PAGE_SIZE)),
            None => self
                .pages
                .lock()
                .pending
                .as_ref()
                .map(|p| (p.origin.clone(), p.base + offset / PAGE_SIZE)),
        };
        let mut absent = Ranges::new();
        in_pieces(0, len / PAGE_SIZE, |from, to| {
            self.share_pages(&clone, offset, (from, to), budget, work, &mut absent)
        })?;
        if !absent.is_empty() {
            let (origin, base) = origin.ok_or(Error::Internal)?;
            budget.charge_kernel(absent.len() as u64 * PAGE_ENTRY_COST)?;
            let pending = Pending::new(origin, base, absent, Some(budget.clone()));
            clone.pages.lock().pending = Some(pending);
        }
        Ok(clone)
    }

    /// Makes the pages `from` to `to`, exclusive, of `clone` use the
    /// frames of the pages from `offset` of this object, which the table
    /// of shared frames counts, and copies the lent ones and, for a paged
    /// object, those that are absent. Each
    /// slot's entry is charged to `budget`. No writable translation of the
    /// shared frames is left in this object's mappings when it returns,
    /// and a failure leaves no use taken that `clone` does not hold.
    fn share_pages<F: FnMut()>(
        self: &Arc<Self>,
        clone: &MemoryObject,
        offset: u64,
        (from, to): (u64, u64),
        budget: &Arc<Budget>,
        work: &mut Work<F>,
        absent: &mut Ranges,
    ) -> Result<(), Error> {
        // A revocation waits for the sharing to end, and fails it when it
        // came first.
        let guard = match &self.paged {
            Some(_) => Some(Supplying::begin(self, || self.check_revoked())?),
            None => None,
        };
        let first = offset / PAGE_SIZE;
        let count = (to - from) as usize;
        let mut shared: Vec<(u64, u64)> = Vec::new();
        let mut lent: Vec<u64> = Vec::new();
        let mut gone: Vec<u64> = Vec::new();
        shared
            .try_reserve_exact(count)
            .map_err(|_| Error::NoMemory)?;
        lent.try_reserve_exact(count).map_err(|_| Error::NoMemory)?;
        gone.try_reserve_exact(count).map_err(|_| Error::NoMemory)?;
        absent.reserve(count)?;
        // The uses are taken with the pages locked and once no output into
        // the source is still to be written, and none of its pages is
        // being changed.
        loop {
            self.wait_unpinned();
            let mut pages = self.pages.lock();
            if pages.pinned() {
                continue;
            }
            if let Some(index) = (first + from..first + to).find(|&i| self.is_changing(i)) {
                drop(pages);
                self.wait_settled(index)?;
                continue;
            }
            pages.check_range((first + from) * PAGE_SIZE, (to - from) * PAGE_SIZE)?;
            shared.clear();
            lent.clear();
            gone.clear();
            let mut failed = None;
            for i in from..to {
                let index = first + i;
                let Some(phys) = pages.frame(index) else {
                    // A page that is absent from a paged object, or that
                    // an anonymous one is still to take from its own
                    // origin, is taken by the clone when it first uses it;
                    // another reads as zero.
                    if self.paged.is_some() || Self::is_pending(&pages, index) {
                        gone.push(i);
                    }
                    continue;
                };
                if crate::share::is_lent(phys) {
                    lent.push(i);
                    continue;
                }
                // The budget that the frame's page is charged to: the
                // object's, or the one that supplied it to a paged object.
                let owner = match &self.paged {
                    None => self.budget.clone(),
                    Some(paged) => match paged.charges.lock().get(&phys) {
                        Some(Charge::Supplied(owner)) => owner.clone(),
                        _ => {
                            lent.push(i);
                            continue;
                        }
                    },
                };
                if let Err(e) = crate::share::copy(phys, owner.as_ref(), Some(budget)) {
                    failed = Some(e);
                    break;
                }
                pages.mark_shared(index);
                shared.push((i, phys));
            }
            drop(pages);
            match failed {
                None => break,
                // A kernel write into a frame of a lend group is under way:
                // look again once it ends.
                Some(Error::BadState) => {
                    Self::end_copies(&shared);
                    let _ = crate::sched::sleep_until(crate::arch::monotonic_now() + 1_000_000);
                }
                Some(e) => {
                    Self::end_copies(&shared);
                    return Err(e);
                }
            }
        }
        // Room for the clone's slots and their entries, which the caller
        // pays for.
        let mut claims = Vec::new();
        let charged = shared.len() as u64 * PAGE_ENTRY_COST;
        let reserved = claims
            .try_reserve_exact(shared.len())
            .map_err(|_| Error::NoMemory)
            .and_then(|()| {
                shared.iter().try_for_each(|_| {
                    claims.push(nodes::claim_insert()?);
                    Ok(())
                })
            })
            .and_then(|()| budget.charge_kernel(charged));
        if let Err(e) = reserved {
            Self::end_copies(&shared);
            return Err(e);
        }
        // The source's translations of the pages go before the call
        // returns, so that no store reaches the shared frames from now on.
        self.unmap_in_steps(first + from, first + to);
        {
            let mut cpages = clone.pages.lock();
            for (&(i, phys), claim) in shared.iter().zip(claims) {
                cpages.insert_copy(i, phys, claim);
            }
        }
        drop(guard);
        for &i in &gone {
            absent.push(i, i + 1);
        }
        // What cannot be shared is copied.
        for i in lent {
            self.copy_pages(
                clone,
                (first + i) * PAGE_SIZE,
                PAGE_SIZE,
                i * PAGE_SIZE,
                budget,
                work,
            )?;
        }
        Ok(())
    }

    /// Ends the uses of the frames of `shared`, taken for a clone that was
    /// not made.
    fn end_copies(shared: &[(u64, u64)]) {
        for &(_, phys) in shared {
            crate::share::end(phys, crate::share::Kind::Copy);
        }
    }

    /// Copies the `len` bytes from `offset` of this object into `clone` from
    /// `at`, all at once, in pieces that stay within one page of each
    /// object; pieces that read as zero are left without a frame. The
    /// copy waits until no output into this object is still to be
    /// written.
    fn copy_pages<F: FnMut()>(
        self: &Arc<Self>,
        clone: &MemoryObject,
        offset: u64,
        len: u64,
        at: u64,
        budget: &Arc<Budget>,
        work: &mut Work<F>,
    ) -> Result<(), Error> {
        let held = self.hold_for_read(offset, len, true, Some(budget), work)?;
        let mut piece = [0u8; PAGE_SIZE as usize];
        let mut done = 0;
        let mut copied = Ok(());
        while done < len && copied.is_ok() {
            let from = offset + done;
            let n = (PAGE_SIZE - from % PAGE_SIZE)
                .min(PAGE_SIZE - (at + done) % PAGE_SIZE)
                .min(len - done) as usize;
            let bytes = &mut piece[..n];
            held.read(from, bytes);
            if bytes.iter().any(|&b| b != 0) {
                let mut rest: &[u8] = bytes;
                copied = clone.write(at + done, n as u64, work, |to| {
                    let (now, later) = rest.split_at(to.len());
                    to.copy_from_slice(now);
                    rest = later;
                    Ok(())
                });
            }
            done += n as u64;
            work.item();
        }
        held.release(work);
        copied
    }

    /// A paged object of `size` bytes created with `options`, charged to
    /// `budget`, that sends its page requests to `port` with `key` and is
    /// supplied by the pager with object ID `pager`.
    pub fn new_paged(
        budget: &Arc<Budget>,
        pager: u64,
        port: &Arc<Port>,
        key: u64,
        size: u64,
        options: u32,
    ) -> Result<Arc<MemoryObject>, Error> {
        if options & !(RESIZABLE | TRACK_DIRTY | STABLE) != 0
            || (options & STABLE != 0 && options & TRACK_DIRTY == 0)
        {
            return Err(Error::InvalidArgs);
        }
        let pages = Pages::new(size, options & RESIZABLE, max_size())?;
        let mut state = Paging::new(options & TRACK_DIRTY != 0);
        if options & STABLE != 0 {
            state = state.stable();
        }
        // With its entry in the pager's list of objects and that entry's
        // share of storage.
        let charged = (size_of::<MemoryObject>() + 2 * size_of::<Weak<MemoryObject>>()) as u64;
        budget.charge_kernel(charged)?;
        // The object releases the charge when it goes, also on failure.
        crate::fallible::try_arc(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: Some(budget.clone()),
            charged,
            pages: SpinLock::new(crate::lock_order::MEMORY_PAGES, pages),
            physical: None,
            mappings: SpinLock::new(crate::lock_order::MEMORY_MAPPINGS, nodes::map()),
            writable: AtomicU64::new(0),
            changing: SpinLock::new(crate::lock_order::MEMORY_CHANGING, Vec::new()),
            seals: AtomicU32::new(0),
            supplies: AtomicU32::new(0),
            paged: Some(Paged {
                pager,
                port: Arc::downgrade(port),
                key,
                state: SpinLock::new(
                    crate::lock_order::MEMORY_PAGING,
                    state.charged_to(budget.clone()),
                ),
                revoked: AtomicBool::new(false),
                charges: SpinLock::new(crate::lock_order::MEMORY_CHARGES, nodes::map()),
                writebacks: SpinLock::new(crate::lock_order::MEMORY_WRITEBACK, Vec::new()),
                uses: SpinLock::new(crate::lock_order::MEMORY_USES, 0),
            }),
            fixed: None,
        })
        .inspect(|object| {
            // No use exists yet, so the object starts unreferenced.
            object
                .header
                .update_signals_quietly(0, crate::object::signals::UNREFERENCED);
        })
    }

    /// Records one more use of the object, a handle without the right
    /// `MANAGE` or a mapping: a paged object stops being unreferenced
    /// before the call that makes the use returns.
    pub(crate) fn use_begin(&self) {
        let Some(paged) = &self.paged else {
            return;
        };
        let mut uses = paged.uses.lock();
        *uses += 1;
        if *uses == 1 {
            self.header
                .update_signals(crate::object::signals::UNREFERENCED, 0);
        }
    }

    /// Records the end of a use, and sets the signal when it was the last.
    pub(crate) fn use_end(&self) {
        let Some(paged) = &self.paged else {
            return;
        };
        let mut uses = paged.uses.lock();
        debug_assert!(*uses > 0, "a use ended that did not begin");
        *uses -= 1;
        if *uses == 0 {
            self.header
                .update_signals(0, crate::object::signals::UNREFERENCED);
        }
    }

    /// How many uses the object has, 0 for an object that is not paged.
    #[cfg(test)]
    pub(crate) fn uses(&self) -> u64 {
        self.paged.as_ref().map_or(0, |paged| *paged.uses.lock())
    }

    pub fn is_paged(&self) -> bool {
        self.paged.is_some()
    }

    /// Whether the object is neither physical, paged, contiguous nor
    /// pinned.
    pub fn is_anonymous(&self) -> bool {
        self.physical.is_none() && self.paged.is_none() && self.fixed.is_none()
    }

    /// Whether the object is contiguous or pinned.
    pub fn is_fixed(&self) -> bool {
        self.fixed.is_some()
    }

    /// Fails with `AccessDenied` when the object is revoked.
    pub fn check_revoked(&self) -> Result<(), Error> {
        match &self.paged {
            Some(p) if p.revoked.load(Ordering::Acquire) => Err(Error::AccessDenied),
            _ => Ok(()),
        }
    }

    /// Whether `pager` is the pager of this paged object.
    pub fn paged_by(&self, pager: &dyn Object) -> bool {
        self.paged
            .as_ref()
            .is_some_and(|p| p.pager == pager.header().koid())
    }

    /// Queues a request for page `index` on the object's port, charged to
    /// the object's budget until it is read.
    fn request(&self, paged: &Paged, index: u64) -> Result<(), Error> {
        let mut payload = [0u8; 24];
        payload[0..4].copy_from_slice(&PAGER_READ.to_le_bytes());
        payload[8..16].copy_from_slice(&(index * PAGE_SIZE).to_le_bytes());
        payload[16..24].copy_from_slice(&PAGE_SIZE.to_le_bytes());
        let packet = crate::port::packet(paged.key, PACKET_PAGER, 0, &payload);
        let port = paged.port.upgrade().ok_or(Error::PeerClosed)?;
        let budget = self.budget.as_ref().expect("a paged object has a budget");
        port.queue_kernel(packet, budget)
    }

    /// Waits until page `index` of a paged object is present, requesting
    /// it when nobody has. Fails with `Io` when the page fails, its pager
    /// is gone or its request cannot be queued, and with `AccessDenied`
    /// once the object is revoked, and with `NoResources` when the wait
    /// cannot be charged. Anonymous objects return at once.
    pub fn obtain(&self, index: u64) -> Result<(), Error> {
        let Some(paged) = &self.paged else {
            return Ok(());
        };
        let koid = self.header.koid();
        let mut charge = None;
        loop {
            let pages = self.pages.lock();
            if index >= pages.count() {
                return Err(Error::OutOfRange);
            }
            let mut state = paged.state.lock();
            if state.revoked() {
                return Err(Error::AccessDenied);
            }
            if pages.frame(index).is_some() {
                return Ok(());
            }
            if charge.is_none() {
                charge = Some(crate::pager::charge_wait()?);
            }
            let absent = state.absent(index)?;
            // A page that is requested already is waited for from here,
            // under the locks that the answer to the request takes before
            // its wake: registered after they are dropped, the thread
            // could miss an answer that came in between.
            let registered = match absent {
                Absent::Wait => Some(crate::pager::wait_for(koid, index)?),
                _ => None,
            };
            drop(state);
            drop(pages);
            // The thread waits before the page is requested, so that the
            // pager's answer wakes it however soon it comes; a wake before
            // the sleep makes the sleep return at once.
            let thread = match absent {
                Absent::Request => match crate::pager::wait_for(koid, index) {
                    Ok(thread) => thread,
                    Err(e) => {
                        // Not requested after all; the next access does.
                        paged.state.lock().withdraw(index);
                        crate::pager::wake(koid, index, index + 1);
                        return Err(e);
                    }
                },
                Absent::Wait => registered.expect("registered under the locks"),
                Absent::Failed => return Err(Error::Io),
                Absent::Revoked => return Err(Error::AccessDenied),
            };
            if let Absent::Request = absent {
                let failed = match self.request(paged, index) {
                    Ok(()) => None,
                    // The page is requested again by the next access,
                    // also by the threads waiting for it.
                    Err(Error::NoResources) => {
                        paged.state.lock().withdraw(index);
                        Some(Error::NoResources)
                    }
                    Err(_) => {
                        // Without room to record the failure, the next
                        // access requests the page again.
                        let _ = paged.state.lock().fail(index, index + 1);
                        Some(Error::Io)
                    }
                };
                if let Some(e) = failed {
                    crate::pager::stop_waiting(koid, &thread);
                    crate::pager::wake(koid, index, index + 1);
                    return Err(e);
                }
            }
            let slept = crate::sched::wait_for_signals(&thread, i64::MAX);
            crate::pager::stop_waiting(koid, &thread);
            slept?;
            if thread.state() == crate::sched::DEAD {
                return Err(Error::Io);
            }
        }
    }

    /// Waits until page `index` of a paged object may be written, which
    /// it may not while a writeback of a `STABLE` object records it.
    /// Fails with `Io` when the pager is gone, as accesses fail after
    /// `FAIL`, with `AccessDenied` once the object is revoked, with `Io`
    /// when the thread is killed meanwhile, and with `NoResources` when
    /// the wait cannot be charged.
    pub fn wait_stable(&self, index: u64) -> Result<(), Error> {
        let Some(paged) = &self.paged else {
            return Ok(());
        };
        let koid = self.header.koid();
        let _charge = crate::pager::charge_wait()?;
        loop {
            let pages = self.pages.lock();
            let state = paged.state.lock();
            if state.revoked() {
                return Err(Error::AccessDenied);
            }
            if state.is_pager_gone() {
                return Err(Error::Io);
            }
            if !state.write_waits(index) {
                return Ok(());
            }
            // The end of the writeback needs the state, so its wake comes
            // after the thread waits.
            let thread = crate::pager::wait_for(koid, index)?;
            drop(state);
            drop(pages);
            let slept = crate::sched::wait_for_signals(&thread, i64::MAX);
            crate::pager::stop_waiting(koid, &thread);
            slept?;
            if thread.state() == crate::sched::DEAD {
                return Err(Error::Io);
            }
        }
    }

    /// Makes every unanswered and later page request fail, as the
    /// object's pager is gone.
    pub fn pager_gone(&self) {
        if let Some(paged) = &self.paged {
            let _pages = self.pages.lock();
            paged.state.lock().pager_gone();
        }
        crate::pager::wake(self.header.koid(), 0, u64::MAX);
    }

    /// Waits until no kernel copy holds the object's frames, so that
    /// nothing prepared before can still read or write them.
    fn wait_unpinned(&self) {
        while self.pages.lock().pinned() {
            let _ = crate::sched::sleep_until(crate::arch::monotonic_now() + 1_000_000);
        }
    }

    /// Waits until no `pager_supply` call moves frames out of the object
    /// or into it.
    fn wait_supplies(&self) {
        while self.supplies.load(Ordering::Relaxed) > 0 {
            let _ = crate::sched::sleep_until(crate::arch::monotonic_now() + 1_000_000);
        }
    }

    /// Runs `op` of `pager_op` on the pages of the `len` bytes from
    /// `offset` of a paged object.
    pub fn pager_op(&self, op: u32, offset: u64, len: u64) -> Result<(), Error> {
        let paged = self.paged.as_ref().ok_or(Error::NotSupported)?;
        if !(OP_FAIL..=OP_DISCARD).contains(&op) {
            return Err(Error::InvalidArgs);
        }
        self.pages.lock().check_range(offset, len)?;
        let (first, end) = (offset / PAGE_SIZE, (offset + len).div_ceil(PAGE_SIZE));
        match op {
            OP_FAIL => in_pieces(first, end, |from, to| {
                let result = paged.state.lock().fail(from, to);
                // The waiting threads are woken also when not every page
                // could be failed: those whose request was forgotten
                // request again.
                crate::pager::wake(self.header.koid(), from, to);
                result
            }),
            OP_EVICT => in_pieces(first, end, |from, to| {
                self.evict(paged, from, to);
                Ok(())
            }),
            OP_DISCARD => {
                // No page of the range is in a writeback, and none begins
                // over it until every page of it is gone.
                {
                    let _pages = self.pages.lock();
                    paged.state.lock().discard_begin(first, end)?;
                }
                let result = in_pieces(first, end, |from, to| {
                    self.discard(paged, from, to);
                    Ok(())
                });
                let _pages = self.pages.lock();
                paged.state.lock().discard_done(first, end);
                result
            }
            OP_WRITEBACK_BEGIN => {
                // A writeback begins over pages being discarded once they
                // are gone.
                loop {
                    let pages = self.pages.lock();
                    pages.check_range(offset, len)?;
                    let mut state = paged.state.lock();
                    match state.writeback_open(first, end) {
                        Ok(()) => {}
                        Err(Error::ShouldWait) => {
                            drop(state);
                            drop(pages);
                            let _ =
                                crate::sched::sleep_until(crate::arch::monotonic_now() + 1_000_000);
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                    let mut ranges = paged.writebacks.lock();
                    if ranges.try_reserve(1).is_err() {
                        state.writeback_cancel(first, end);
                        return Err(Error::NoMemory);
                    }
                    ranges.push((offset, len));
                    break;
                }
                // The dirty pages are recorded in steps, each once no
                // kernel copy holds the frames: a page being written must
                // not go unrecorded.
                loop {
                    let pages = self.pages.lock();
                    if pages.pinned() {
                        drop(pages);
                        self.wait_unpinned();
                        continue;
                    }
                    let mut state = paged.state.lock();
                    let (recorded, done) = match state.writeback_record(first, end, ITEM_STEP) {
                        Ok(step) => step,
                        Err(e) => {
                            paged.writebacks.lock().retain(|&r| r != (offset, len));
                            return Err(e);
                        }
                    };
                    drop(state);
                    drop(pages);
                    // Writes through mappings fault again and are seen.
                    // Writes before the translations go come before the
                    // pager reads the pages, after this call.
                    if let (Some(&low), Some(&high)) = (recorded.first(), recorded.last()) {
                        self.unmap_in_steps(low, high + 1);
                    }
                    if done {
                        return Ok(());
                    }
                    crate::sched::preempt_point();
                }
            }
            _ => {
                {
                    let mut state = paged.state.lock();
                    state.need_dirty_tracking()?;
                    if !paged.writebacks.lock().contains(&(offset, len)) {
                        return Err(Error::BadState);
                    }
                    state.writeback_close(first, end, op == OP_WRITEBACK_END)?;
                }
                // The recorded pages are handled in steps; the writeback
                // stays outstanding until the last. Writes that waited for
                // its pages go on once it has ended.
                loop {
                    let mut state = paged.state.lock();
                    if state.writeback_finish(first, end, ITEM_STEP) {
                        paged.writebacks.lock().retain(|&r| r != (offset, len));
                        drop(state);
                        crate::pager::wake(self.header.koid(), first, end);
                        return Ok(());
                    }
                    drop(state);
                    crate::sched::preempt_point();
                }
            }
        }
    }

    /// Evicts the evictable pages from `first` to `end`, exclusive, as
    /// `pager_op` `EVICT` does, once no kernel copy holds the frames: a
    /// page being written must not go. Pages without room to record
    /// their frame stay.
    fn evict(&self, paged: &Paged, first: u64, end: u64) {
        self.remove(paged, first, end, Paging::evictable);
    }

    /// Removes every present page from `first` to `end`, exclusive, dirty
    /// or clean, as `pager_op` `DISCARD` does, once no kernel copy holds
    /// the frames. No writeback involves any of them, which
    /// `discard_begin` made sure of.
    fn discard(&self, paged: &Paged, first: u64, end: u64) {
        self.remove(paged, first, end, Paging::present);
    }

    /// Removes the pages from `first` to `end`, exclusive, that `which`
    /// picks, once no kernel copy holds the frames, and frees their
    /// frames once their translations are gone. Pages without room to
    /// record their frame stay.
    fn remove(&self, paged: &Paged, first: u64, end: u64, which: fn(&Paging, u64) -> bool) {
        let mut pages = loop {
            self.wait_unpinned();
            let pages = self.pages.lock();
            if !pages.pinned() {
                break pages;
            }
        };
        let end = end.min(pages.count());
        let mut state = paged.state.lock();
        // The frames listed keep their charges until they are freed, after
        // the list goes, so those cover it.
        let mut freed = Vec::new();
        for index in first..end {
            if !which(&state, index) || freed.try_reserve(1).is_err() {
                continue;
            }
            if let Ok(taken) = pages.decommit(index, index + 1) {
                freed.extend(taken);
                state.removed(index);
            }
        }
        drop(state);
        drop(pages);
        // The translations go before the frames.
        self.unmap_in_steps(first, end);
        self.free(freed);
    }

    /// The dirty ranges of the `len` bytes from `offset`, as byte offsets
    /// and lengths of whole pages, as `pager_query_dirty` finds them, up
    /// to the first `cap`. The dirty pages are found in steps.
    pub fn dirty_ranges(
        &self,
        offset: u64,
        len: u64,
        cap: u64,
        scratch: &mut Scratch,
    ) -> Result<Vec<(u64, u64)>, Error> {
        let paged = self.paged.as_ref().ok_or(Error::NotSupported)?;
        {
            let pages = self.pages.lock();
            pages.check_range(offset, len)?;
            paged.state.lock().need_dirty_tracking()?;
        }
        let (mut at, end) = (offset / PAGE_SIZE, (offset + len).div_ceil(PAGE_SIZE));
        // Page index ranges, one more than `cap` once the last is known
        // to have ended.
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        while at < end && ranges.len() as u64 <= cap {
            let state = paged.state.lock();
            let mut seen = 0;
            for index in state.dirty(at, end).take(ITEM_STEP) {
                seen += 1;
                at = index + 1;
                match ranges.last_mut() {
                    Some(r) if r.1 == index => r.1 += 1,
                    _ => {
                        scratch.reserve(&mut ranges, 1)?;
                        ranges.push((index, index + 1));
                    }
                }
            }
            drop(state);
            if seen < ITEM_STEP {
                break;
            }
            crate::sched::preempt_point();
        }
        ranges.truncate(ranges.len().min(cap as usize));
        for (offset, len) in &mut ranges {
            (*offset, *len) = (*offset * PAGE_SIZE, (*len - *offset) * PAGE_SIZE);
        }
        Ok(ranges)
    }

    /// Revokes the whole paged object for good, as `pager_revoke` does:
    /// waits for kernel copies that hold its frames and for supplies into
    /// it, removes every translation of it, and wakes the threads waiting
    /// for its pages.
    pub fn revoke(&self) -> Result<(), Error> {
        let paged = self.paged.as_ref().ok_or(Error::NotSupported)?;
        let count = loop {
            self.wait_unpinned();
            self.wait_supplies();
            let pages = self.pages.lock();
            if pages.pinned() || self.supplies.load(Ordering::Relaxed) > 0 {
                continue;
            }
            paged.state.lock().revoke();
            paged.revoked.store(true, Ordering::Release);
            break pages.count();
        };
        // No page can be entered once revoked, and the size stays, so the
        // translations go in pieces.
        let _ = in_pieces(0, count, |from, to| {
            self.unmap_in_steps(from, to);
            Ok(())
        });
        crate::pager::wake(self.header.koid(), 0, u64::MAX);
        Ok(())
    }

    /// Moves the frames of the `len` bytes from `source_offset` of
    /// `source` into the pages from `offset` that are not present, as
    /// `pager_supply` does, and frees the rest, so that the source range
    /// ends up decommitted. Moved frames stay charged to the source's
    /// budget. A `WRITE` seal of the source and a revocation of this
    /// object that come meanwhile wait until the call ends.
    pub fn supply(
        &self,
        offset: u64,
        len: u64,
        source: &MemoryObject,
        source_offset: u64,
    ) -> Result<(), Error> {
        let Some(paged) = &self.paged else {
            return Err(Error::NotSupported);
        };
        if !source.is_anonymous() {
            return Err(Error::NotSupported);
        }
        let _out_of = Supplying::begin(source, || source.check_writable())?;
        let _into = Supplying::begin(self, || self.check_revoked())?;
        let aligned = [offset, len, source_offset]
            .iter()
            .all(|v| v.is_multiple_of(PAGE_SIZE));
        if !aligned {
            return Err(Error::InvalidArgs);
        }
        self.check_range(offset, len)?;
        source.check_range(source_offset, len)?;
        let (first, source_first) = (offset / PAGE_SIZE, source_offset / PAGE_SIZE);
        in_pieces(0, len / PAGE_SIZE, |from, to| {
            let source_offset = (source_first + from) * PAGE_SIZE;
            let len = (to - from) * PAGE_SIZE;
            // Room for every frame of the piece that does not fit, before
            // any frame is taken. The frames keep their charges until they
            // are freed, after the list goes, so those cover it.
            let mut leftover = Vec::new();
            leftover
                .try_reserve_exact((to - from) as usize)
                .map_err(|_| Error::NoMemory)?;
            // Frames move only once no kernel copy holds the source;
            // nothing waits between the commit and the move.
            let moved = loop {
                source.wait_unpinned();
                source.commit(source_offset, len)?;
                match source.take_frames(source_offset, len) {
                    Err(Error::BadState) => continue,
                    taken => break taken?,
                }
            };
            let (from, to) = (first + from, first + to);
            // Once the object's budget refuses a page, the rest of the
            // piece stays with the source and the call fails.
            let mut refused = Ok(());
            {
                let mut pages = self.pages.lock();
                let mut state = paged.state.lock();
                let mut charges = paged.charges.lock();
                for (index, taken) in (from..).zip(moved) {
                    // A frame that pages use as a lent frame stays with
                    // them, and a copy moves instead.
                    let Some(phys) = source.movable(taken) else {
                        leftover.push(taken);
                        refused = Err(Error::NoMemory);
                        continue;
                    };
                    let fits = index < pages.count() && pages.frame(index).is_none();
                    let claims = fits
                        .then(|| Some((nodes::claim_insert().ok()?, nodes::claim_insert().ok()?)))
                        .flatten();
                    let supplied = match claims {
                        Some(claims) if refused.is_ok() => match state.supply(index) {
                            Ok(true) => Some(claims),
                            Err(Error::NoResources) => {
                                refused = Err(Error::NoResources);
                                None
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    // A frame that stays itself keeps its mark.
                    let moved = if phys == taken.phys() {
                        taken
                    } else {
                        Frame::private(phys)
                    };
                    if let Some((charge, page)) = supplied {
                        charges.insert(phys, Charge::Supplied(source.budget.clone()));
                        drop(charge);
                        if moved.is_copy() {
                            pages.insert_copy(index, phys, page);
                        } else if moved.is_shared() {
                            pages.insert_shared(index, phys, page);
                        } else {
                            pages.insert(index, phys, page);
                        }
                    } else {
                        leftover.push(moved);
                    }
                }
            }
            source.free(leftover);
            crate::pager::wake(self.header.koid(), from, to);
            refused
        })
    }

    /// Makes the pages of the `len` bytes from `offset` that are not
    /// present use the frames of the `len` bytes from `source_offset` of
    /// `source`, which keeps them and their charges, as `pager_lend`
    /// does: the source's pages get frames first, and the frames stay
    /// allocated while a page uses them (`crate::share`). Lent pages are
    /// clean, and their requests answered. Fails with `AccessDenied` when
    /// the source has the `WRITE` seal or this object is revoked, as for
    /// `supply`, whose seal and revocation checks this shares.
    pub fn lend(
        self: &Arc<Self>,
        offset: u64,
        len: u64,
        source: &Arc<MemoryObject>,
        source_offset: u64,
    ) -> Result<(), Error> {
        let Some(paged) = &self.paged else {
            return Err(Error::NotSupported);
        };
        if !source.is_anonymous() {
            return Err(Error::NotSupported);
        }
        let _out_of = Supplying::begin(source, || source.check_writable())?;
        let _into = Supplying::begin(self, || self.check_revoked())?;
        let aligned = [offset, len, source_offset]
            .iter()
            .all(|v| v.is_multiple_of(PAGE_SIZE));
        if !aligned {
            return Err(Error::InvalidArgs);
        }
        self.check_range(offset, len)?;
        source.check_range(source_offset, len)?;
        let (first, source_first) = (offset / PAGE_SIZE, source_offset / PAGE_SIZE);
        in_pieces(0, len / PAGE_SIZE, |from, to| {
            let count = (to - from) as usize;
            let mut frames = Vec::new();
            frames
                .try_reserve_exact(count)
                .map_err(|_| Error::NoMemory)?;
            let mut unused = Vec::new();
            unused
                .try_reserve_exact(count)
                .map_err(|_| Error::NoMemory)?;
            // The frames are recorded as lent with the source's pages
            // locked, so that the source frees none of them before.
            loop {
                source.commit((source_first + from) * PAGE_SIZE, (to - from) * PAGE_SIZE)?;
                // Frames are lent once no kernel copy holds the source, and
                // none of its pages is changing.
                source.wait_unpinned();
                let mut pages = source.pages.lock();
                if pages.pinned() {
                    continue;
                }
                if let Some(index) =
                    (source_first + from..source_first + to).find(|&i| source.is_changing(i))
                {
                    drop(pages);
                    source.wait_settled(index)?;
                    continue;
                }
                // A frame that slots of clones share is not lent as it is:
                // the paged object's writes would reach them. The page
                // gets a frame of its own first, and so does the slot of a
                // clone, which is not the owner of its frame.
                let copied = (source_first + from..source_first + to).find(|&index| {
                    pages.frame(index).is_some_and(|phys| {
                        source.class(&pages, index, phys) == Write::Break || pages.is_copy(index)
                    })
                });
                if let Some(index) = copied {
                    if source.is_changing(index) {
                        drop(pages);
                        source.wait_settled(index)?;
                    } else {
                        source.break_slot(&mut pages, index)?;
                        drop(pages);
                        source.settle(index, index + 1);
                    }
                    continue;
                }
                let found: Option<Vec<u64>> = (source_first + from..source_first + to)
                    .map(|index| pages.frame(index))
                    .collect();
                // A page decommitted since the commit gets a frame again.
                let Some(found) = found else {
                    continue;
                };
                for (index, phys) in (source_first + from..).zip(found) {
                    if let Err(e) =
                        crate::share::lend(phys, source.budget.as_ref(), Some((source, index)))
                    {
                        drop(pages);
                        frames
                            .into_iter()
                            .for_each(|phys| crate::share::end(phys, crate::share::Kind::Lent));
                        return Err(e);
                    }
                    // The source's slot now has an entry in the table.
                    pages.mark_shared(index);
                    frames.push(phys);
                }
                break;
            }
            let (from, to) = (first + from, first + to);
            // Once the object's budget refuses to record a page, the rest
            // of the piece is not lent and the call fails, as for supply.
            let mut refused = Ok(());
            {
                let mut pages = self.pages.lock();
                let mut state = paged.state.lock();
                let mut charges = paged.charges.lock();
                for (index, phys) in (from..).zip(frames) {
                    let fits = index < pages.count() && pages.frame(index).is_none();
                    let claims = fits
                        .then(|| Some((nodes::claim_insert().ok()?, nodes::claim_insert().ok()?)))
                        .flatten();
                    let lent = match claims {
                        Some(claims) if refused.is_ok() => match state.supply(index) {
                            Ok(true) => Some(claims),
                            Err(Error::NoResources) => {
                                refused = Err(Error::NoResources);
                                None
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    let Some((charge, page)) = lent else {
                        unused.push(phys);
                        continue;
                    };
                    // The page joins the frame's lend group.
                    if crate::share::landing(phys, self, index).is_err() {
                        unused.push(phys);
                        continue;
                    }
                    match charges.get_mut(&phys) {
                        Some(Charge::Lent(n)) => *n += 1,
                        _ => {
                            charges.insert(phys, Charge::Lent(1));
                        }
                    }
                    drop(charge);
                    pages.insert_shared(index, phys, page);
                }
            }
            unused
                .into_iter()
                .for_each(|phys| crate::share::end(phys, crate::share::Kind::Lent));
            crate::pager::wake(self.header.koid(), from, to);
            refused
        })
    }

    /// The frame to move in place of `taken`, which was taken out of this
    /// anonymous object: itself, unless pages of paged objects use it as
    /// a lent frame or slots of clones share it. Then it stays with them,
    /// with its charge, and a copy charged to this object's budget moves
    /// instead. `None` without a frame or a charge for the copy.
    fn movable(&self, taken: Frame) -> Option<u64> {
        let phys = taken.phys();
        let shared = crate::share::is_lent(phys)
            || if taken.is_copy() {
                crate::share::uses(phys) > 1
            } else {
                crate::share::has_copies(phys)
            };
        if !shared {
            return Some(phys);
        }
        let copy = self.copy_frame(phys).ok()?;
        // This object's use of the original ends: the frame stays while
        // others use it, and otherwise it is freed with its charges.
        self.free([taken]);
        Some(copy)
    }

    /// Takes the frames of the `len` bytes from `offset`, all present,
    /// out of the object without freeing them or releasing their charges,
    /// and removes their translations. Fails with `BadState` while a
    /// reader or writer holds the object's frames.
    fn take_frames(&self, offset: u64, len: u64) -> Result<Vec<Frame>, Error> {
        let mut pages = self.pages.lock();
        if pages.pinned() {
            return Err(Error::BadState);
        }
        let (first, end) = (offset / PAGE_SIZE, (offset + len) / PAGE_SIZE);
        let taken = pages.decommit(first, end)?;
        drop(pages);
        // No translation of the frames is left when they move.
        self.unmap_in_steps(first, end);
        Ok(taken)
    }

    /// A physical object of the `size` bytes from `base`, both multiples
    /// of the page size, mapped with `policy`, charged to `budget` if
    /// given.
    pub fn new_physical(
        budget: Option<&Arc<Budget>>,
        base: u64,
        size: u64,
        policy: Policy,
    ) -> Result<Arc<MemoryObject>, Error> {
        debug_assert!(base.is_multiple_of(PAGE_SIZE) && size.is_multiple_of(PAGE_SIZE));
        let pages = Pages::new(size, 0, max_size())?;
        let charged = match budget {
            Some(budget) => {
                let size = size_of::<MemoryObject>() as u64;
                budget.charge_kernel(size)?;
                size
            }
            None => 0,
        };
        // The object releases the charge when it goes, also on failure.
        crate::fallible::try_arc(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.cloned(),
            charged,
            pages: SpinLock::new(crate::lock_order::MEMORY_PAGES, pages),
            physical: Some((base, policy)),
            mappings: SpinLock::new(crate::lock_order::MEMORY_MAPPINGS, nodes::map()),
            writable: AtomicU64::new(0),
            changing: SpinLock::new(crate::lock_order::MEMORY_CHANGING, Vec::new()),
            seals: AtomicU32::new(0),
            supplies: AtomicU32::new(0),
            paged: None,
            fixed: None,
        })
    }

    /// A contiguous object of `size` bytes, a multiple of the page size,
    /// whose zeroed frames start at a multiple of 2 to the power of
    /// `align_log2` and, with `below_4g`, end below 4 GiB, charged to
    /// `budget`. Fails with `NoMemory` when no such run is free.
    pub fn new_contiguous(
        budget: &Arc<Budget>,
        size: u64,
        align_log2: u32,
        below_4g: bool,
    ) -> Result<Arc<MemoryObject>, Error> {
        let count = size / PAGE_SIZE;
        let limit = if below_4g { 1 << 32 } else { u64::MAX };
        Self::new_fixed(budget, size, |object| {
            object.charge(count)?;
            let align = 1 << (align_log2 - PAGE_SIZE.trailing_zeros());
            let Some(first) = crate::frames::alloc_contiguous(count, align, limit) else {
                object.release(count);
                return Err(Error::NoMemory);
            };
            let frames = (0..count).map(|i| first + i * PAGE_SIZE);
            let mut work = Work::new(crate::sched::preempt_point);
            for phys in frames.clone() {
                work.item();
                // SAFETY: the frame was just allocated, so nothing else
                // references it.
                unsafe { frame_bytes(phys) }.fill(0);
            }
            // Once inserted, the object frees the frames it holds and
            // releases their charges; the others are freed here.
            let mut pages = object.pages.lock();
            for (index, phys) in frames.enumerate() {
                match nodes::claim_insert() {
                    Ok(claim) => pages.insert(index as u64, phys, claim),
                    Err(e) => {
                        drop(pages);
                        object.free(
                            (index as u64..count).map(|i| Frame::private(first + i * PAGE_SIZE)),
                        );
                        return Err(e);
                    }
                }
            }
            Ok(Fixed::Contiguous(first))
        })
    }

    /// A pinned object of `size` bytes, a multiple of the page size, whose
    /// zeroed frames are allocated now and, with `below_4g`, lie below
    /// 4 GiB, charged to `budget`.
    pub fn new_pinned(
        budget: &Arc<Budget>,
        size: u64,
        below_4g: bool,
    ) -> Result<Arc<MemoryObject>, Error> {
        Self::new_fixed(budget, size, |object| {
            let mut work = Work::new(crate::sched::preempt_point);
            for index in 0..size / PAGE_SIZE {
                work.item();
                let phys = object.alloc(&mut object.pages.lock(), index)?;
                // The allocator gives the lowest free frame, so none is
                // left below 4 GiB once one lies above.
                if below_4g && phys >= 1 << 32 {
                    return Err(Error::NoMemory);
                }
            }
            Ok(Fixed::Pinned)
        })
    }

    /// An object of `size` bytes charged to `budget` whose pages `fill`
    /// gives frames, returning its kind. When `fill` fails, the object is
    /// destroyed with the frames it was given.
    fn new_fixed(
        budget: &Arc<Budget>,
        size: u64,
        fill: impl FnOnce(&MemoryObject) -> Result<Fixed, Error>,
    ) -> Result<Arc<MemoryObject>, Error> {
        let mut object = Self::new(Some(budget), size, 0)?;
        let fixed = fill(&object)?;
        // Nothing else holds the object yet.
        Arc::get_mut(&mut object).expect("a new object").fixed = Some(fixed);
        Ok(object)
    }

    /// Charges `count` frames and their page entries to the budget, if
    /// the object has one.
    fn charge(&self, count: u64) -> Result<(), Error> {
        if let Some(budget) = &self.budget {
            budget.charge_pages(count)?;
            if let Err(e) = budget.charge_kernel(count * PAGE_ENTRY_COST) {
                budget.release_pages(count);
                return Err(e);
            }
        }
        Ok(())
    }

    /// The physical address of the start of a physical or contiguous
    /// object, or `NotSupported` for other kinds.
    pub fn physical_start(&self) -> Result<u64, Error> {
        match (self.physical, self.fixed) {
            (Some((base, _)), _) => Ok(base),
            (_, Some(Fixed::Contiguous(first))) => Ok(first),
            _ => Err(Error::NotSupported),
        }
    }

    /// The physical addresses of the `count` pages from page `first` of a
    /// contiguous or pinned object, none when `count` is 0. Fails with
    /// `NotSupported` for other kinds and `OutOfRange` when the pages lie
    /// beyond the object.
    pub fn physical_pages(&self, first: u64, count: u64) -> Result<Vec<u64>, Error> {
        if self.fixed.is_none() {
            return Err(Error::NotSupported);
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let pages = self.pages.lock();
        let end = first.checked_add(count).ok_or(Error::OutOfRange)?;
        if end > pages.count() {
            return Err(Error::OutOfRange);
        }
        let mut out = Vec::new();
        out.try_reserve(count as usize)
            .map_err(|_| Error::NoMemory)?;
        out.extend((first..end).map(|i| pages.frame(i).expect("fixed pages have frames")));
        Ok(out)
    }

    pub fn seals(&self) -> u32 {
        self.seals.load(Ordering::Acquire)
    }

    /// Fails with `AccessDenied` when the object has the `WRITE` seal.
    pub fn check_writable(&self) -> Result<(), Error> {
        if self.seals() & SEAL_WRITE != 0 {
            return Err(Error::AccessDenied);
        }
        Ok(())
    }

    /// Adds `seals`, as `mem_seal` does.
    pub fn seal(&self, seals: u32) -> Result<(), Error> {
        if seals & !SEALS != 0 {
            return Err(Error::InvalidArgs);
        }
        if !self.is_anonymous() {
            return Err(Error::NotSupported);
        }
        if self.seals() & SEAL_SEAL != 0 {
            return Err(Error::AccessDenied);
        }
        let write = seals & SEAL_WRITE != 0;
        // A write seal waits until no kernel copy holds the frames, so
        // that no output prepared before it is written after it, and
        // until no supply moves frames out of the object.
        let held = loop {
            if write {
                self.wait_unpinned();
                self.wait_supplies();
            }
            let pages = self.pages.lock();
            if !write || (!pages.pinned() && self.supplies.load(Ordering::Relaxed) == 0) {
                break pages;
            }
        };
        if write && self.mapped_writable() {
            return Err(Error::BadState);
        }
        // Writes through the pages that use a lent frame change the
        // object (MIKSPEC-0006.R53). Frames are lent with the pages
        // locked, as they are here.
        if write && held.any_frame(crate::share::is_lent) {
            return Err(Error::BadState);
        }
        self.seals.fetch_or(seals, Ordering::AcqRel);
        Ok(())
    }

    /// Whether a region of some address space maps the object writable.
    /// The pages must be locked.
    fn mapped_writable(&self) -> bool {
        self.writable.load(Ordering::Relaxed) > 0
    }

    pub fn is_physical(&self) -> bool {
        self.physical.is_some()
    }

    /// An anonymous object of `count` pages that takes over the `count`
    /// frames from `phys`, charging them to no budget, as the kernel does
    /// this at boot. The frames return to the frame allocator when the
    /// object is destroyed.
    pub fn from_frames(phys: u64, count: u64) -> Result<Arc<MemoryObject>, Error> {
        MemoryObject::from_frame_runs(count * PAGE_SIZE, &[(0, phys, count)])
    }

    /// An anonymous object of `size` bytes, a multiple of the page size,
    /// that takes over the frames of `runs`, each given as the page index
    /// it starts at and the `count` frames from `phys`, charging them to no
    /// budget, as the kernel does this at boot. Its other pages are zero
    /// until written. The frames return to the frame allocator when the
    /// object is destroyed.
    pub fn from_frame_runs(
        size: u64,
        runs: &[(u64, u64, u64)],
    ) -> Result<Arc<MemoryObject>, Error> {
        let frames: Vec<(u64, u64)> = runs
            .iter()
            .flat_map(|&(index, phys, count)| {
                (0..count).map(move |i| (index + i, phys + i * PAGE_SIZE))
            })
            .collect();
        // The frames come back to the allocator with the object.
        if let Some(allocator) = FRAMES.lock().as_mut() {
            allocator.adopt(frames.len() as u64);
        }
        let object = MemoryObject::new(None, size, 0)?;
        let mut pages = object.pages.lock();
        for (at, &(index, phys)) in frames.iter().enumerate() {
            match nodes::claim_insert() {
                Ok(claim) => pages.insert(index, phys, claim),
                Err(e) => {
                    // The object frees the frames it took, and the rest
                    // go back here.
                    drop(pages);
                    object.free(frames[at..].iter().map(|&(_, phys)| Frame::private(phys)));
                    return Err(e);
                }
            }
        }
        drop(pages);
        Ok(object)
    }

    /// Fails with `OutOfRange` unless the `len` bytes from `offset` lie
    /// inside the object.
    pub fn check_range(&self, offset: u64, len: u64) -> Result<(), Error> {
        self.pages.lock().check_range(offset, len)
    }

    /// The record of a region of `space` from `start` that maps the
    /// object, writable when `writable`, or `NoMemory` without room for
    /// it.
    pub fn record(
        self: &Arc<Self>,
        space: &Arc<AddressSpace>,
        start: u64,
        writable: bool,
    ) -> Result<Mapping, Error> {
        let id = NEXT_RECORD.fetch_add(1, Ordering::Relaxed);
        Mapping::add(self, (id, start), Arc::downgrade(space), writable)
    }

    /// Removes the translations of pages `first` to `end`, exclusive, of
    /// the object from every region that maps it, in steps with reschedule
    /// checks. No lock may be held.
    ///
    /// The caller has changed the pages first, so that translations made
    /// meanwhile reflect the change and may stay. Only the regions with
    /// records from before the start are visited: a newer region has only
    /// such translations, and a split keeps its record's ID, so a part
    /// split off a region not visited yet is still visited.
    fn unmap_in_steps(&self, first: u64, end: u64) {
        let last = NEXT_RECORD.load(Ordering::Relaxed);
        let mut next = (0, 0);
        let mut visited = 0;
        loop {
            // The mappings are visited one at a time, none of them locked
            // while a region is.
            let entry = {
                let mappings = self.mappings.lock();
                let entry = mappings.range(next..(last, 0)).next();
                entry.map(|(&key, space)| (key, space.clone()))
            };
            let Some(((id, start), space)) = entry else {
                return;
            };
            next = (id, start + 1);
            if let Some(space) = space.upgrade() {
                space.unmap_mapping_in_steps(self, start, first, end);
            }
            visited += 1;
            if visited % ITEM_STEP == 0 {
                crate::sched::preempt_point();
            }
        }
    }

    /// Gives page `index` a frame in place of the shared zero page, and
    /// marks it changing until `settle` has removed the translations of
    /// the zero page that stood in for it (a page whose frame is shared
    /// is marked changing the same way by `break_slot`).
    fn alloc_changing(&self, pages: &mut Pages, index: u64) -> Result<u64, Error> {
        self.mark(index, index + 1)?;
        self.alloc(pages, index).inspect_err(|_| {
            self.unmark(index, index + 1);
        })
    }

    /// Marks pages `first` to `end` as changing, or fails with `NoMemory`
    /// without room to. The pages are locked.
    fn mark(&self, first: u64, end: u64) -> Result<(), Error> {
        let mut changing = self.changing.lock();
        changing.try_reserve(1).map_err(|_| Error::NoMemory)?;
        changing.push(Changing {
            first,
            end,
            old: None,
        });
        Ok(())
    }

    /// Ends the mark of `mark` on pages `first` to `end`, and wakes the
    /// threads waiting for them. Returns the frame that the change left
    /// and that the caller ends the slot's use of.
    fn unmark(&self, first: u64, end: u64) -> Option<Frame> {
        let mut changing = self.changing.lock();
        let at = changing
            .iter()
            .position(|m| (m.first, m.end) == (first, end));
        let old = changing.swap_remove(at.expect("the pages were marked")).old;
        drop(changing);
        crate::pager::wake(self.header.koid(), first, end);
        old
    }

    /// Records that the frame `old` is what the change of pages `first` to
    /// `end`, which are marked, left, for `settle` to end the use of.
    fn leave(&self, first: u64, end: u64, old: Frame) {
        let mut changing = self.changing.lock();
        let mark = changing
            .iter_mut()
            .find(|m| (m.first, m.end) == (first, end))
            .expect("the pages were marked");
        debug_assert!(mark.old.is_none());
        mark.old = Some(old);
    }

    /// Removes the old translations of pages `first` to `end`, marked by
    /// `mark` and then changed, ends the mark, and ends the use of the
    /// frame that the change left, if it left one. No lock may be held.
    fn settle(&self, first: u64, end: u64) {
        self.unmap_in_steps(first, end);
        if let Some(old) = self.unmark(first, end) {
            self.free([old]);
        }
    }

    /// Whether a write to page `index`, which has the frame `phys`, needs
    /// a frame of its own, because slots of objects made by `mem_clone`
    /// still see the bytes of this one, or, for a slot of such an object,
    /// because another use does. A slot that was never marked
    /// shared is not looked up in the table. The pages are locked.
    fn needs_copy(&self, pages: &Pages, index: u64, phys: u64) -> bool {
        self.class(pages, index, phys) != Write::InPlace
    }

    /// How a write to page `index`, which has the frame `phys`, goes: in
    /// place, with a frame of its own for the page, or, for a page of a
    /// lend group, once the group moved to a new frame. The pages are
    /// locked.
    fn class(&self, pages: &Pages, index: u64, phys: u64) -> Write {
        if !pages.is_shared(index) {
            return Write::InPlace;
        }
        crate::share::write_class(phys, pages.is_copy(index))
    }

    /// Gives page `index`, whose frame is shared, a frame of its own with
    /// the same bytes, charged as any frame of the pages is, and marks the
    /// page changing until `settle` has removed the translations of the
    /// old frame, which it then ends the slot's use of. While the pages
    /// are pinned the old frame is kept until the last `unpin` instead.
    /// Fails, changing nothing, with `NoMemory` or `NoResources`. The
    /// pages are locked.
    fn break_slot(&self, pages: &mut Pages, index: u64) -> Result<(), Error> {
        let old = pages
            .frame(index)
            .expect("the page of a shared frame has one");
        debug_assert!(pages.is_shared(index));
        // The copy of a paged object's frame is charged to the budget
        // that the frame was supplied with, as any of its frames is.
        let supplied = match &self.paged {
            None => None,
            Some(paged) => match paged.charges.lock().get(&old) {
                Some(Charge::Supplied(owner)) => Some(owner.clone()),
                _ => return Err(Error::Internal),
            },
        };
        let owner = supplied.as_ref().unwrap_or(&self.budget);
        self.mark(index, index + 1)?;
        let pinned = pages.pinned();
        let claim = match supplied {
            Some(_) => nodes::claim_insert().map(Some),
            None => Ok(None),
        };
        let copy = match claim.and_then(|claim| {
            pages.reserve_orphan()?;
            let copy = self.copy_frame_to(old, owner.as_ref())?;
            Ok((claim, copy))
        }) {
            Ok(done) => done,
            Err(e) => {
                self.unmark(index, index + 1);
                return Err(e);
            }
        };
        let (claim, copy) = copy;
        if let (Some(paged), Some(owner)) = (&self.paged, supplied) {
            paged.charges.lock().insert(copy, Charge::Supplied(owner));
        }
        drop(claim);
        // The slot keeps its place in the pages, which need no room.
        let left = pages.replace(index, copy, false);
        if pinned {
            pages.orphan(left).expect("room was reserved");
        } else {
            self.leave(index, index + 1, left);
        }
        Ok(())
    }

    /// A new frame with the bytes of the frame `phys`, charged to this
    /// object's budget as any frame of its pages is, or `NoResources`
    /// without room in the budget and `NoMemory` without a frame.
    fn copy_frame(&self, phys: u64) -> Result<u64, Error> {
        self.copy_frame_to(phys, self.budget.as_ref())
    }

    /// A new frame with the bytes of the frame `phys`, charged to
    /// `budget`, or `NoResources` without room in it and `NoMemory`
    /// without a frame.
    fn copy_frame_to(&self, phys: u64, budget: Option<&Arc<Budget>>) -> Result<u64, Error> {
        if let Some(budget) = budget {
            budget.charge_pages(1)?;
            if let Err(e) = budget.charge_kernel(PAGE_ENTRY_COST) {
                budget.release_pages(1);
                return Err(e);
            }
        }
        let Some(copy) = crate::frames::alloc_frame() else {
            if let Some(budget) = budget {
                budget.release_pages(1);
                budget.release_kernel(PAGE_ENTRY_COST);
            }
            return Err(Error::NoMemory);
        };
        // SAFETY: the copy was just allocated, and the frame being copied
        // stays allocated while this object's slot uses it. Writes through
        // the pages that share it may change it meanwhile, which only
        // changes the bytes copied.
        unsafe {
            core::ptr::copy_nonoverlapping(
                crate::arch::frame_address(phys) as *const u8,
                crate::arch::frame_address(copy) as *mut u8,
                PAGE_SIZE as usize,
            );
        }
        Ok(copy)
    }

    /// The object's koid.
    pub(crate) fn koid(&self) -> u64 {
        self.header.koid()
    }

    /// Whether page `index` is marked changing. The pages are locked.
    fn is_changing(&self, index: u64) -> bool {
        let changing = self.changing.lock();
        changing.iter().any(|m| (m.first..m.end).contains(&index))
    }

    /// Waits until page `index` is not marked changing. Fails with `Io`
    /// when the thread is killed meanwhile, and with `NoResources` when
    /// the wait cannot be charged.
    pub fn wait_settled(&self, index: u64) -> Result<(), Error> {
        let koid = self.header.koid();
        let _charge = crate::pager::charge_wait()?;
        loop {
            // The thread registers before it looks at the mark: the end of
            // a mark does not take the pages' lock, so a wake that came
            // between the look and the registration would find nobody to
            // wake. Registered first, a wake that comes after wakes the
            // thread, and one that came before ended the mark, which the
            // look then sees.
            let thread = crate::pager::wait_for(koid, index)?;
            let pages = self.pages.lock();
            if !self.is_changing(index) {
                drop(pages);
                crate::pager::stop_waiting(koid, &thread);
                return Ok(());
            }
            drop(pages);
            let slept = crate::sched::wait_for_signals(&thread, i64::MAX);
            crate::pager::stop_waiting(koid, &thread);
            slept?;
            if thread.state() == crate::sched::DEAD {
                return Err(Error::Io);
            }
        }
    }

    /// Settles page `index`, which a fault gave a frame and marked.
    pub fn settle_page(&self, index: u64) {
        self.settle(index, index + 1);
    }

    /// Whether page `index` is still to come from the paged origin of
    /// this clone. The pages are locked.
    fn is_pending(pages: &Pages, index: u64) -> bool {
        pages
            .pending
            .as_ref()
            .is_some_and(|pending| pending.ranges.contains(index))
    }

    /// Gives page `index` of this clone, if it is pending, the frame that
    /// the origin has for it, which it asks the origin's pager for as an
    /// access to the origin does, and ends its being pending. The access
    /// fails as an access to the origin would, and the page stays
    /// pending. Waits for the pager with no lock held.
    pub fn resolve_pending(&self, index: u64) -> Result<(), Error> {
        loop {
            let (origin, at) = {
                let pages = self.pages.lock();
                match &pages.pending {
                    Some(p) if p.ranges.contains(index) => (p.origin.clone(), p.base + index),
                    _ => return Ok(()),
                }
            };
            origin.obtain(at)?;
            // As `mem_clone` of the origin does, frames are shared once no
            // kernel copy holds the origin and its page is not changing.
            origin.wait_unpinned();
            let claim = nodes::claim_insert()?;
            // The origin's frame is used from now on, as a clone's slot.
            let mut taken =
                {
                    // A revocation waits for the sharing, or came first and
                    // fails it.
                    let _supply = Supplying::begin(&origin, || origin.check_revoked())?;
                    let mut pages = origin.pages.lock();
                    if pages.pinned() {
                        drop(pages);
                        continue;
                    }
                    if origin.is_changing(at) {
                        drop(pages);
                        origin.wait_settled(at)?;
                        continue;
                    }
                    let Some(phys) = pages.frame(at) else {
                        // Gone again since it was obtained.
                        drop(pages);
                        continue;
                    };
                    let owner = origin.paged.as_ref().and_then(|paged| {
                        match paged.charges.lock().get(&phys) {
                            Some(Charge::Supplied(owner)) => Some(owner.clone()),
                            _ => None,
                        }
                    });
                    match owner {
                        Some(owner) => {
                            match crate::share::copy(phys, owner.as_ref(), self.budget.as_ref()) {
                                Ok(()) => {}
                                // A kernel write into the frame is under way.
                                Err(Error::BadState) => {
                                    drop(pages);
                                    let _ = crate::sched::sleep_until(
                                        crate::arch::monotonic_now() + 1_000_000,
                                    );
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                            if let Some(budget) = &self.budget
                                && let Err(e) = budget.charge_kernel(PAGE_ENTRY_COST)
                            {
                                crate::share::end(phys, crate::share::Kind::Copy);
                                return Err(e);
                            }
                            pages.mark_shared(at);
                            drop(pages);
                            // No store through a mapping of the origin
                            // may reach the frame that the clone now uses.
                            origin.unmap_in_steps(at, at + 1);
                            Taken::Shared(phys)
                        }
                        // A lent frame is copied, as a clone's pages are.
                        None => Taken::Copied(self.copy_frame(phys)?),
                    }
                };
            let mut pages = self.pages.lock();
            let ended = match pages.pending.as_mut() {
                Some(p) if p.ranges.contains(index) => match p.cut(index, index + 1) {
                    Ok(()) => Some(p.ranges.is_empty()),
                    Err(e) => {
                        drop(pages);
                        self.give_back(core::mem::replace(&mut taken, Taken::Nothing));
                        return Err(e);
                    }
                },
                // Another thread, or a decommit, was first.
                _ => None,
            };
            match (ended, core::mem::replace(&mut taken, Taken::Nothing)) {
                (Some(done), Taken::Shared(phys)) => {
                    pages.insert_copy(index, phys, claim);
                    let origin = if done { pages.pending.take() } else { None };
                    drop(pages);
                    drop(origin);
                    return Ok(());
                }
                (Some(done), Taken::Copied(phys)) => {
                    pages.insert(index, phys, claim);
                    let origin = if done { pages.pending.take() } else { None };
                    drop(pages);
                    drop(origin);
                    return Ok(());
                }
                (_, other) => {
                    drop(pages);
                    self.give_back(other);
                    return Ok(());
                }
            }
        }
    }

    /// Ends the use of a frame that `resolve_pending` took and did not
    /// need after all.
    fn give_back(&self, taken: Taken) {
        match taken {
            Taken::Nothing => {}
            Taken::Shared(phys) => {
                crate::share::end(phys, crate::share::Kind::Copy);
                if let Some(budget) = &self.budget {
                    budget.release_kernel(PAGE_ENTRY_COST);
                }
            }
            Taken::Copied(phys) => {
                self.free([Frame::private(phys)]);
            }
        }
    }

    /// Enters the page at `virt` of `space` into its tables for a fault
    /// that was a write or a fetch as given. Pages read before they were
    /// written map the shared zero page read-only. Fails with `Denied`
    /// unless the region at `virt`, which maps this object, allows the
    /// access.
    pub fn fault(&self, space: &AddressSpace, virt: u64, write: bool, fetch: bool) -> Fault {
        let mut pages = self.pages.lock();
        let mine = |r: Option<&aspace::RegionOf>| {
            r.filter(|r| core::ptr::eq(&*r.mem, self))
                .map(|r| (r.page(virt), r.perms))
        };
        let Some((index, perms)) = space.with_region(virt, mine) else {
            return Fault::Denied;
        };
        if !aspace::allows(perms, write, fetch) || index >= pages.count() {
            return Fault::Denied;
        }
        if let Some(paged) = &self.paged {
            let mut state = paged.state.lock();
            if state.revoked() {
                return Fault::Denied;
            }
            let Some(phys) = pages.frame(index) else {
                return Fault::Absent(index);
            };
            if self.is_changing(index) {
                return Fault::Busy(index);
            }
            if write && state.write_waits(index) {
                return Fault::Stable(index);
            }
            // A page whose frame clones share gets one of its own on a
            // store, and until then is mapped to be read only.
            let class = self.class(&pages, index, phys);
            let shared = class != Write::InPlace;
            if shared && write {
                drop(state);
                if class == Write::Group {
                    return Fault::Group(phys);
                }
                return match self.break_slot(&mut pages, index) {
                    Ok(()) => Fault::Changed(index),
                    Err(_) => Fault::OutOfMemory,
                };
            }
            // With dirty tracking, a page becomes writable through a
            // mapping only on a write, which marks it dirty.
            let writable = !shared
                && perms & PERM_WRITE != 0
                && (write || state.writable_without_fault(index));
            if write && state.written(index).is_err() {
                return Fault::OutOfMemory;
            }
            drop(state);
            let access = Access {
                writable,
                executable: perms & PERM_EXECUTE != 0,
                policy: Policy::Cached,
            };
            return match space.replace(virt, phys, access) {
                Ok(()) => Fault::Resolved,
                Err(_) => Fault::OutOfMemory,
            };
        }
        if self.physical.is_none() && self.is_changing(index) {
            return Fault::Busy(index);
        }
        let (phys, writable) = match (self.physical, pages.frame(index)) {
            (Some((base, _)), _) => (base + index * PAGE_SIZE, perms & PERM_WRITE != 0),
            (None, Some(phys)) if self.needs_copy(&pages, index, phys) => {
                // A store gets a frame of its own, and a load maps the
                // shared one so that the next store faults.
                if write {
                    if self.class(&pages, index, phys) == Write::Group {
                        return Fault::Group(phys);
                    }
                    return match self.break_slot(&mut pages, index) {
                        Ok(()) => Fault::Changed(index),
                        Err(_) => Fault::OutOfMemory,
                    };
                }
                (phys, false)
            }
            (None, Some(phys)) => (phys, perms & PERM_WRITE != 0),
            (None, None) if Self::is_pending(&pages, index) => return Fault::Pending(index),
            // The page is entered once the zero page is gone everywhere.
            (None, None) if write => match self.alloc_changing(&mut pages, index) {
                Ok(_) => return Fault::Changed(index),
                Err(_) => return Fault::OutOfMemory,
            },
            (None, None) => match zero_frame() {
                Some(phys) => (phys, false),
                None => return Fault::OutOfMemory,
            },
        };
        let access = Access {
            writable,
            executable: perms & PERM_EXECUTE != 0,
            policy: self.physical.map_or(Policy::Cached, |(_, policy)| policy),
        };
        // The region may have gone while the zero page was being removed.
        if !space.with_region(virt, |r| mine(r) == Some((index, perms))) {
            return Fault::Denied;
        }
        match space.replace(virt, phys, access) {
            Ok(()) => Fault::Resolved,
            Err(_) => Fault::OutOfMemory,
        }
    }

    /// Holds the page at `virt` of `space`, which this object backs, as
    /// the destination of a kernel write that cannot fail later: gives it
    /// a frame if it has none and keeps that frame until the result is
    /// dropped, or maps a physical page into the kernel with the object's
    /// policy. Fails with `InvalidArgs` unless the region at `virt` maps
    /// this object writable.
    pub fn hold_for_write(
        self: &Arc<Self>,
        space: &AddressSpace,
        virt: u64,
    ) -> Result<Held, Error> {
        self.hold_mapped(space, virt, true)
    }

    /// Holds the page at `virt` of `space` as `hold_for_write` does, as
    /// the source of a kernel read that cannot fail later. Fails with
    /// `InvalidArgs` unless the region at `virt` maps this object readable.
    pub fn hold_for_read_at(
        self: &Arc<Self>,
        space: &AddressSpace,
        virt: u64,
    ) -> Result<Held, Error> {
        self.hold_mapped(space, virt, false)
    }

    fn hold_mapped(
        self: &Arc<Self>,
        space: &AddressSpace,
        virt: u64,
        write: bool,
    ) -> Result<Held, Error> {
        loop {
            let pages = self.pages.lock();
            let mine = |r: Option<&aspace::RegionOf>| {
                r.filter(|r| core::ptr::eq(&*r.mem, &**self))
                    .map(|r| (r.page(virt), r.perms))
            };
            let Some((index, perms)) = space.with_region(virt, mine) else {
                return Err(Error::InvalidArgs);
            };
            if !aspace::allows(perms, write, false) || index >= pages.count() {
                return Err(Error::InvalidArgs);
            }
            if let Some(held) = self.hold_page(pages, index, write)? {
                return Ok(held);
            }
        }
    }

    /// Holds page `index` as the destination of a debug write, as
    /// `hold_for_write` does whatever maps it. Fails with `AccessDenied`
    /// when the object has the `WRITE` seal or is revoked, with
    /// `OutOfRange` when the page lies beyond its end, and with
    /// `NotSupported` for a physical object.
    pub fn hold_for_debug(self: &Arc<Self>, index: u64) -> Result<Held, Error> {
        if self.is_physical() {
            return Err(Error::NotSupported);
        }
        loop {
            let pages = self.pages.lock();
            self.check_writable()?;
            if index >= pages.count() {
                return Err(Error::OutOfRange);
            }
            if let Some(held) = self.hold_page(pages, index, true)? {
                return Ok(held);
            }
        }
    }

    /// Holds page `index` with the pages locked, or waits for what it
    /// needs with them unlocked and returns `None`, so that the caller
    /// looks again. A page of a paged object held to `write` is marked
    /// written.
    fn hold_page(
        self: &Arc<Self>,
        mut pages: crate::sync::Guard<'_, Pages>,
        index: u64,
        write: bool,
    ) -> Result<Option<Held>, Error> {
        if let Some((base, policy)) = self.physical {
            drop(pages);
            let virt = crate::arch::physical_page_map(base + index * PAGE_SIZE, policy)
                .ok_or(Error::NoMemory)?;
            return Ok(Some(Held {
                mem: self.clone(),
                backing: Some(Backing::Mapped(virt)),
                counted: false,
            }));
        }
        if let Some(paged) = &self.paged {
            let mut state = paged.state.lock();
            if state.revoked() {
                return Err(Error::AccessDenied);
            }
            let Some(phys) = pages.frame(index) else {
                drop(state);
                drop(pages);
                self.obtain(index)?;
                return Ok(None);
            };
            if self.is_changing(index) {
                drop(state);
                drop(pages);
                self.wait_settled(index)?;
                return Ok(None);
            }
            if write && state.write_waits(index) {
                drop(state);
                drop(pages);
                self.wait_stable(index)?;
                return Ok(None);
            }
            if write && self.needs_copy(&pages, index, phys) {
                drop(state);
                if self.class(&pages, index, phys) == Write::Group {
                    drop(pages);
                    move_group(phys)?;
                    return Ok(None);
                }
                self.break_slot(&mut pages, index)?;
                drop(pages);
                self.settle(index, index + 1);
                return Ok(None);
            }
            let counted = match write && pages.is_shared(index) && !pages.is_copy(index) {
                true => match crate::share::hold(phys) {
                    crate::share::Hold::Plain => false,
                    crate::share::Hold::Counted => true,
                    crate::share::Hold::Moved => {
                        drop(state);
                        drop(pages);
                        move_group(phys)?;
                        return Ok(None);
                    }
                },
                false => false,
            };
            if write && let Err(e) = state.written(index) {
                if counted {
                    crate::share::unhold(phys);
                }
                return Err(e);
            }
            drop(state);
            pages.pin();
            return Ok(Some(Held {
                mem: self.clone(),
                backing: Some(Backing::Frame(phys)),
                counted,
            }));
        }
        if self.is_changing(index) {
            drop(pages);
            self.wait_settled(index)?;
            return Ok(None);
        }
        let Some(phys) = pages.frame(index) else {
            if Self::is_pending(&pages, index) {
                drop(pages);
                self.resolve_pending(index)?;
                return Ok(None);
            }
            self.alloc_changing(&mut pages, index)?;
            drop(pages);
            self.settle(index, index + 1);
            return Ok(None);
        };
        if write && self.needs_copy(&pages, index, phys) {
            if self.class(&pages, index, phys) == Write::Group {
                drop(pages);
                move_group(phys)?;
                return Ok(None);
            }
            self.break_slot(&mut pages, index)?;
            drop(pages);
            self.settle(index, index + 1);
            return Ok(None);
        }
        let counted = match write && pages.is_shared(index) && !pages.is_copy(index) {
            true => match crate::share::hold(phys) {
                crate::share::Hold::Plain => false,
                crate::share::Hold::Counted => true,
                crate::share::Hold::Moved => {
                    drop(pages);
                    move_group(phys)?;
                    return Ok(None);
                }
            },
            false => false,
        };
        pages.pin();
        Ok(Some(Held {
            mem: self.clone(),
            backing: Some(Backing::Frame(phys)),
            counted,
        }))
    }

    /// Gives every page a frame and keeps the frames until `unpin_all`,
    /// even when the object shrinks meanwhile, and returns them in page
    /// order with the object's size. Fails with `WrongType` unless the
    /// object is anonymous: a paged object's frames come from its pager.
    #[cfg(feature = "fuzz")]
    pub fn pin_all(&self) -> Result<(Vec<u64>, u64), Error> {
        if !self.is_anonymous() {
            return Err(Error::WrongType);
        }
        let mut pages = self.pages.lock();
        if pages.pending.is_some() {
            return Err(Error::WrongType);
        }
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(pages.count() as usize)
            .map_err(|_| Error::NoMemory)?;
        let count = pages.count();
        self.mark(0, count)?;
        let mut result = Ok(());
        for index in 0..count {
            let phys = match pages.frame(index) {
                Some(phys) => phys,
                None => match self.alloc(&mut pages, index) {
                    Ok(phys) => phys,
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                },
            };
            frames.push(phys);
        }
        if result.is_ok() {
            pages.pin();
        }
        let size = pages.size();
        drop(pages);
        self.settle(0, count);
        result.map(|()| (frames, size))
    }

    /// Ends a pin of `pin_all`.
    #[cfg(feature = "fuzz")]
    pub fn unpin_all(&self) {
        self.unpin(&mut Work::new(|| {}));
    }

    /// Records the frame of every page of the `len` bytes from `offset`,
    /// keeping each until the result is released, so that they can be read
    /// without the pages locked. Pages without a frame read as zeros and
    /// get none. The record is charged to `budget`. Fails with `OutOfRange`
    /// when the range stops lying inside the object meanwhile. When
    /// `exclusive`, the frames are recorded only once no other kernel copy
    /// holds them, so that no write prepared before is still to come.
    pub fn hold_for_read<F: FnMut()>(
        self: &Arc<Self>,
        offset: u64,
        len: u64,
        exclusive: bool,
        budget: Option<&Arc<Budget>>,
        work: &mut Work<F>,
    ) -> Result<Source, Error> {
        debug_assert!(!self.is_physical());
        let first = offset / PAGE_SIZE;
        let count = match len {
            0 => 0,
            _ => (offset + len).div_ceil(PAGE_SIZE) - first,
        };
        let bytes = count * size_of::<Option<u64>>() as u64;
        if let Some(budget) = budget {
            budget.charge_kernel(bytes)?;
        }
        let mut source = Source {
            mem: self.clone(),
            offset,
            frames: Vec::new(),
            pinned: false,
            charged: budget.map(|budget| (budget.clone(), bytes)),
            copies: Vec::new(),
            copies_charged: None,
        };
        if source.frames.try_reserve_exact(count as usize).is_err() {
            return Err(Error::NoMemory);
        }
        if let Some(paged) = &self.paged {
            // Every page is obtained first, then all are recorded together
            // with the pages locked, so that none goes in between.
            loop {
                for index in first..first + count {
                    work.item();
                    self.obtain(index)?;
                }
                if exclusive {
                    self.wait_unpinned();
                }
                let mut pages = self.pages.lock();
                if exclusive && pages.pinned() {
                    continue;
                }
                if paged.state.lock().revoked() {
                    return Err(Error::AccessDenied);
                }
                pages.check_range(offset, len)?;
                if (first..first + count).all(|i| pages.frame(i).is_some()) {
                    source
                        .frames
                        .extend((first..first + count).map(|i| pages.frame(i)));
                    pages.pin();
                    source.pinned = true;
                    return Ok(source);
                }
            }
        }
        loop {
            if exclusive {
                self.wait_unpinned();
            }
            let mut pages = self.pages.lock();
            if !exclusive || !pages.pinned() {
                pages.pin();
                break;
            }
        }
        source.pinned = true;
        for index in first..first + count {
            work.item();
            let mut pages = self.pages.lock();
            // A pending page is taken from the origin first.
            while Self::is_pending(&pages, index) {
                drop(pages);
                if let Err(e) = self.resolve_pending(index) {
                    source.release(work);
                    return Err(e);
                }
                pages = self.pages.lock();
            }
            let found = pages.check_range(offset, len).map(|()| pages.frame(index));
            drop(pages);
            match found {
                Ok(frame) => source.frames.push(frame),
                Err(e) => {
                    source.release(work);
                    return Err(e);
                }
            }
        }
        Ok(source)
    }

    /// Ends one pin of the pages, and frees the frames that were taken
    /// from them while pinned once none is left.
    fn unpin<F: FnMut()>(&self, work: &mut Work<F>) {
        let orphans = self.pages.lock().unpin();
        for batch in orphans.chunks(ITEM_STEP) {
            for _ in batch {
                work.item();
            }
            self.free(batch.iter().copied());
        }
    }

    /// Gives page `index` a zeroed frame, charging the frame to the
    /// budget, if the object has one, and returns it.
    fn alloc(&self, pages: &mut Pages, index: u64) -> Result<u64, Error> {
        let claim = nodes::claim_insert()?;
        if let Some(budget) = &self.budget {
            budget.charge_pages(1)?;
            if let Err(e) = budget.charge_kernel(PAGE_ENTRY_COST) {
                budget.release_pages(1);
                return Err(e);
            }
        }
        let Some(phys) = crate::frames::alloc_frame() else {
            self.release(1);
            return Err(Error::NoMemory);
        };
        // SAFETY: the frame was just allocated, so nothing references it.
        unsafe { frame_bytes(phys) }.fill(0);
        pages.insert(index, phys, claim);
        Ok(phys)
    }

    /// Releases the charges of `count` frames, if the object has a budget.
    fn release(&self, count: u64) {
        if let Some(budget) = &self.budget {
            budget.release_pages(count);
            budget.release_kernel(count * PAGE_ENTRY_COST);
        }
    }

    /// Frees frames taken from the pages and releases their charges.
    ///
    /// Nothing that can free heap memory runs while the frame allocator
    /// is locked, as the heap gives pages back to it: frames are taken
    /// from `frames`, whose iterator may free memory, in batches with the
    /// allocator unlocked, and charges are released after each batch.
    ///
    /// When no lock is held, as when an object is destroyed by closing
    /// its last handle, a reschedule check follows every `ITEM_STEP`
    /// frames.
    fn free(&self, frames: impl IntoIterator<Item = Frame>) {
        const BATCH: usize = 64;
        let mut frames = frames.into_iter();
        let mut count = 0;
        let mut since_check = 0;
        loop {
            if since_check >= ITEM_STEP {
                crate::sched::preempt_point_if_unlocked();
                since_check = 0;
            }
            let mut batch = [0u64; BATCH];
            let mut copies = [false; BATCH];
            let mut len = 0;
            for ((slot, copy), frame) in batch.iter_mut().zip(&mut copies).zip(frames.by_ref()) {
                *slot = frame.phys();
                *copy = frame.is_copy();
                len += 1;
            }
            if len == 0 {
                break;
            }
            since_check += len;
            // What each frame leaves: those lent to this object give their
            // use back, and those that pages of others still use stay, with
            // their page charge, until those let go.
            let mut give: [Give; BATCH] = [const { Give::Nothing }; BATCH];
            for (i, &phys) in batch[..len].iter().enumerate() {
                let mut supplied = None;
                if let Some(paged) = &self.paged {
                    let mut charges = paged.charges.lock();
                    if let Some(Charge::Lent(n)) = charges.get_mut(&phys) {
                        let last = *n <= 1;
                        if !last {
                            *n -= 1;
                        } else {
                            charges.remove(&phys);
                        }
                        drop(charges);
                        // None of this object's pages uses the frame as
                        // lent any more.
                        if last {
                            crate::share::forget_members(phys, self.header.koid());
                        }
                        give[i] = match crate::share::release(phys, crate::share::Kind::Lent) {
                            Released::Last(page) => Give::Last { page, entry: None },
                            _ => Give::Nothing,
                        };
                        continue;
                    }
                    if let Some(Charge::Supplied(budget)) = charges.remove(&phys) {
                        supplied = Some(budget);
                    }
                }
                // The slot's entry was charged to the budget the frame was
                // supplied with, or else to this object's.
                let entry = supplied.clone().unwrap_or_else(|| self.budget.clone());
                // A slot of a clone ends a use of its own kind.
                let kind = if copies[i] {
                    crate::share::Kind::Copy
                } else {
                    crate::share::Kind::Owner
                };
                give[i] = match (crate::share::release(phys, kind), supplied) {
                    (Released::Alone, Some(budget)) => Give::Supplied(budget),
                    (Released::Alone, None) => Give::Own,
                    // The slot's entry goes with it, and its page stays.
                    (Released::Used, _) => Give::Entry(entry),
                    (Released::Last(page), _) => Give::Last {
                        page,
                        entry: Some(entry),
                    },
                };
            }
            if let Some(allocator) = FRAMES.lock().as_mut() {
                for (&phys, give) in batch[..len].iter().zip(&give) {
                    if give.frees() {
                        allocator.free(phys, 1);
                    }
                }
            }
            for give in give.iter_mut().take(len) {
                match core::mem::replace(give, Give::Nothing) {
                    Give::Nothing => {}
                    Give::Own => count += 1,
                    Give::Entry(budget) => {
                        if let Some(budget) = budget {
                            budget.release_kernel(PAGE_ENTRY_COST);
                        }
                    }
                    // A supplied frame's charge goes back to its own
                    // budget.
                    Give::Supplied(Some(budget)) => {
                        budget.release_pages(1);
                        budget.release_kernel(PAGE_ENTRY_COST);
                    }
                    Give::Supplied(None) => {}
                    Give::Last { page, entry } => {
                        if let Some(budget) = page {
                            budget.release_pages(1);
                        }
                        if let Some(Some(budget)) = entry {
                            budget.release_kernel(PAGE_ENTRY_COST);
                        }
                    }
                }
            }
        }
        drop(frames);
        self.release(count);
    }

    /// Fills the `len` bytes from `offset` from `source`, in chunks,
    /// accounting for the copied bytes in `work` with no lock held.
    pub fn write<F: FnMut()>(
        &self,
        offset: u64,
        len: u64,
        work: &mut Work<F>,
        mut source: impl FnMut(&mut [u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.is_physical() {
            return Err(Error::NotSupported);
        }
        self.check_range(offset, len)?;
        let mut done = 0;
        while done < len {
            let (index, inside, n) = chunk(offset + done, len - done);
            work.copy(n);
            let mut buf = [0u8; CHUNK];
            source(&mut buf[..n])?;
            // The chunk is read from the source once; a page of a paged
            // object that went meanwhile is obtained again.
            loop {
                self.obtain(index)?;
                let mut pages = self.pages.lock();
                self.check_writable()?;
                if self.is_changing(index) {
                    drop(pages);
                    self.wait_settled(index)?;
                    continue;
                }
                let phys = match (&self.paged, pages.frame(index)) {
                    (Some(paged), frame) => {
                        let mut state = paged.state.lock();
                        if state.revoked() {
                            return Err(Error::AccessDenied);
                        }
                        let Some(phys) = frame else {
                            continue;
                        };
                        if state.write_waits(index) {
                            drop(state);
                            drop(pages);
                            self.wait_stable(index)?;
                            continue;
                        }
                        if self.needs_copy(&pages, index, phys) {
                            drop(state);
                            if self.class(&pages, index, phys) == Write::Group {
                                drop(pages);
                                move_group(phys)?;
                                continue;
                            }
                            self.break_slot(&mut pages, index)?;
                            drop(pages);
                            self.settle(index, index + 1);
                            continue;
                        }
                        state.written(index)?;
                        phys
                    }
                    (None, Some(phys)) if self.needs_copy(&pages, index, phys) => {
                        if self.class(&pages, index, phys) == Write::Group {
                            drop(pages);
                            move_group(phys)?;
                            continue;
                        }
                        self.break_slot(&mut pages, index)?;
                        drop(pages);
                        self.settle(index, index + 1);
                        continue;
                    }
                    (None, Some(phys)) => phys,
                    (None, None) if Self::is_pending(&pages, index) => {
                        drop(pages);
                        self.resolve_pending(index)?;
                        continue;
                    }
                    (None, None) => {
                        self.alloc_changing(&mut pages, index)?;
                        drop(pages);
                        self.settle(index, index + 1);
                        continue;
                    }
                };
                // SAFETY: the pages are locked.
                let frame = unsafe { frame_bytes(phys) };
                frame[inside..inside + n].copy_from_slice(&buf[..n]);
                break;
            }
            done += n as u64;
        }
        Ok(())
    }

    /// Gives every page of the `len` bytes from `offset` a frame, as
    /// `mem_op` `COMMIT` does.
    pub fn commit(&self, offset: u64, len: u64) -> Result<(), Error> {
        if !self.is_anonymous() {
            return Err(Error::NotSupported);
        }
        self.pages.lock().check_range(offset, len)?;
        let (first, end) = (offset / PAGE_SIZE, (offset + len).div_ceil(PAGE_SIZE));
        in_pieces(first, end, |from, to| {
            // A pending page is taken from the origin, as the first access
            // of the clone would.
            if self.pages.lock().pending.is_some() {
                for index in from..to {
                    self.resolve_pending(index)?;
                }
            }
            let mut pages = self.pages.lock();
            // The object may have shrunk since the last piece.
            pages.check_range(from * PAGE_SIZE, (to - from) * PAGE_SIZE)?;
            self.mark(from, to)?;
            let mut result = Ok(());
            for index in from..to {
                if pages.frame(index).is_none()
                    && let Err(e) = self.alloc(&mut pages, index)
                {
                    result = Err(e);
                    break;
                }
            }
            drop(pages);
            self.settle(from, to);
            result
        })
    }

    /// Frees the frames of every page of the `len` bytes from `offset`,
    /// which then read as zero, as `mem_op` `DECOMMIT` does.
    pub fn decommit(&self, offset: u64, len: u64) -> Result<(), Error> {
        if !self.is_anonymous() {
            return Err(Error::NotSupported);
        }
        {
            let pages = self.pages.lock();
            self.check_writable()?;
            pages.check_range(offset, len)?;
        }
        let (first, end) = (offset / PAGE_SIZE, (offset + len).div_ceil(PAGE_SIZE));
        in_pieces(first, end, |from, to| {
            let mut pages = self.pages.lock();
            self.check_writable()?;
            // Pages the object lost since the last piece have no frames.
            let end = to.min(pages.count());
            if from >= end {
                return Ok(());
            }
            // The pending pages read as zero from now on; they have no
            // frames, so this stands also if freeing the others fails.
            let ended = match pages.pending.as_mut() {
                Some(pending) => {
                    pending.cut(from, end)?;
                    if pending.ranges.is_empty() {
                        pages.pending.take()
                    } else {
                        None
                    }
                }
                None => None,
            };
            let freed = pages.decommit(from, end);
            drop(pages);
            drop(ended);
            let freed = freed?;
            // The translations go before the frames.
            self.unmap_in_steps(from, end);
            self.free(freed);
            Ok(())
        })
    }

    /// Changes the size to `size` bytes, freeing the frames past it.
    ///
    /// The frames past a smaller size are removed in pieces with the
    /// pages unlocked in between. Meanwhile the object's size stays put.
    pub fn set_size(&self, size: u64) -> Result<(), Error> {
        self.check_revoked()?;
        let mut pages = loop {
            let mut pages = self.pages.lock();
            if pages.trimming() {
                drop(pages);
                crate::sched::yield_now();
                continue;
            }
            // The bytes of the last page past a smaller size are zeroed
            // in place, which a frame that slots of other objects see too
            // must not be: it gets a frame of its own first, once the
            // shrink is known to be allowed.
            let shrinks = size != 0
                && size < pages.size()
                && pages.options() & RESIZABLE != 0
                && self.seals() & SEAL_SHRINK == 0;
            let tail = size / PAGE_SIZE;
            match pages.frame(tail) {
                Some(phys)
                    if shrinks
                        && !size.is_multiple_of(PAGE_SIZE)
                        && self.needs_copy(&pages, tail, phys) =>
                {
                    if self.is_changing(tail) {
                        drop(pages);
                        self.wait_settled(tail)?;
                    } else if self.class(&pages, tail, phys) == Write::Group {
                        drop(pages);
                        move_group(phys)?;
                    } else {
                        self.break_slot(&mut pages, tail)?;
                        drop(pages);
                        self.settle(tail, tail + 1);
                    }
                }
                _ => break pages,
            }
        };
        let seals = self.seals();
        let refused = if size < pages.size() {
            seals & SEAL_SHRINK
        } else if size > pages.size() {
            seals & SEAL_GROW
        } else {
            0
        };
        if refused != 0 {
            return Err(Error::AccessDenied);
        }
        let old = pages.count();
        let resized = pages.set_size(size, max_size())?;
        if let Some((phys, from)) = resized.zero {
            // SAFETY: the pages are locked.
            let frame = unsafe { frame_bytes(phys) };
            frame[from..].fill(0);
        }
        let new = pages.count();
        if new >= old {
            return Ok(());
        }
        // Pending pages past the end go: that never needs a new range.
        let ended = match pages.pending.as_mut() {
            Some(pending) => {
                // A tail never splits a range, so this needs no room.
                let cut = pending.cut(new, old);
                debug_assert!(cut.is_ok(), "cutting a tail needs no new range");
                if pending.ranges.is_empty() {
                    pages.pending.take()
                } else {
                    None
                }
            }
            None => None,
        };
        pages.set_trimming(true);
        drop(pages);
        drop(ended);
        let mut removed = resized.removed;
        // The pieces cannot fail: one waits until the frames it cannot
        // keep for kernel copies are no longer used.
        let _ = in_pieces(new, old, |from, to| {
            // The translations go before the frames.
            self.unmap_in_steps(from, to);
            loop {
                let mut pages = self.pages.lock();
                if pages.pinned() {
                    // Kernel copies may still use the frames, which stay
                    // until the last of them ends.
                    let kept = loop {
                        match removed.first_key_value() {
                            Some((&index, &phys)) if index < to => {
                                if pages.orphan(phys).is_err() {
                                    break false;
                                }
                                removed.pop_first();
                            }
                            _ => break true,
                        }
                    };
                    if !kept {
                        drop(pages);
                        self.wait_unpinned();
                        continue;
                    }
                }
                // The pager state learns of every page that left, also
                // while kernel copies hold the frames.
                if let Some(paged) = &self.paged {
                    let mut state = paged.state.lock();
                    for index in from..to {
                        state.removed(index);
                    }
                }
                drop(pages);
                self.free(core::iter::from_fn(|| match removed.first_key_value() {
                    Some((&index, _)) if index < to => removed.pop_first().map(|(_, phys)| phys),
                    _ => None,
                }));
                return Ok(());
            }
        });
        self.pages.lock().set_trimming(false);
        Ok(())
    }

    /// The memory information record.
    pub fn info(&self) -> [u8; INFO_MEMORY_SIZE] {
        let kind = match (self.physical, &self.paged, self.fixed) {
            (Some(_), _, _) => KIND_PHYSICAL,
            (_, Some(_), _) => KIND_PAGED,
            (_, _, Some(Fixed::Contiguous(_))) => KIND_CONTIGUOUS,
            (_, _, Some(Fixed::Pinned)) => KIND_PINNED,
            _ => KIND_ANONYMOUS,
        };
        // The pages keep only `RESIZABLE`; a paged object's paging state
        // keeps whether it was created with `TRACK_DIRTY` and `STABLE`.
        let paging = self.paged.as_ref().map_or(0, |p| {
            let state = p.state.lock();
            (if state.track_dirty() { TRACK_DIRTY } else { 0 })
                | if state.is_stable() { STABLE } else { 0 }
        });
        let pages = self.pages.lock();
        let options = pages.options() | paging;
        pages.info(kind, options, self.seals())
    }
}

/// Where a held page's bytes are reached.
enum Backing {
    /// A frame of the object, through the direct map.
    Frame(u64),
    /// A physical page, mapped into the kernel at this address.
    Mapped(u64),
}

/// A page of a memory object held for writing by `hold_for_write`.
pub struct Held {
    mem: Arc<MemoryObject>,
    /// `None` once released.
    backing: Option<Backing>,
    /// Whether the write is counted in its frame's lend group.
    counted: bool,
}

impl Held {
    fn end<F: FnMut()>(&mut self, work: &mut Work<F>) {
        match self.backing.take() {
            Some(Backing::Frame(phys)) => {
                // Before the pin goes, which keeps the frame allocated.
                if self.counted {
                    crate::share::unhold(phys);
                }
                self.mem.unpin(work)
            }
            // SAFETY: the mapping came from `physical_page_map`, and the hold
            // that used it is gone.
            Some(Backing::Mapped(virt)) => unsafe { crate::arch::physical_page_unmap(virt) },
            None => {}
        }
    }
}

impl crate::publish::Page for Held {
    fn base(&self) -> *mut u8 {
        match self.backing {
            Some(Backing::Frame(phys)) => crate::arch::frame_address(phys) as *mut u8,
            Some(Backing::Mapped(virt)) => virt as *mut u8,
            None => unreachable!("a released page is never written"),
        }
    }

    fn frame(&self) -> Option<u64> {
        match self.backing {
            Some(Backing::Frame(phys)) => Some(phys),
            _ => None,
        }
    }

    fn release<F: FnMut()>(mut self, work: &mut Work<F>) {
        work.item();
        self.end(work);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.end(&mut Work::new(|| {}));
    }
}

/// The frames of a range of an anonymous object recorded by
/// `hold_for_read`, by page from the range's first page.
pub struct Source {
    mem: Arc<MemoryObject>,
    offset: u64,
    frames: Vec<Option<u64>>,
    pinned: bool,
    charged: Option<(Arc<Budget>, u64)>,
    /// Frames the kernel copied recorded pages into, which `frames` then
    /// names in their place, with the budget they are charged to.
    copies: Vec<u64>,
    copies_charged: Option<Arc<Budget>>,
}

impl Source {
    /// Copies the bytes from offset `at` of the object into `buf`, all of
    /// which must come from one recorded page.
    pub fn read(&self, at: u64, buf: &mut [u8]) {
        let index = (at / PAGE_SIZE - self.offset / PAGE_SIZE) as usize;
        let inside = (at % PAGE_SIZE) as usize;
        assert!(inside + buf.len() <= PAGE_SIZE as usize);
        match self.frames[index] {
            // SAFETY: the frame is pinned, so it stays allocated, and the
            // direct map covers it. Writers may change it meanwhile, which
            // only changes the bytes read.
            Some(phys) => unsafe {
                let from = (crate::arch::frame_address(phys) as *const u8).add(inside);
                core::ptr::copy_nonoverlapping(from, buf.as_mut_ptr(), buf.len());
            },
            None => buf.fill(0),
        }
    }

    /// Copies every recorded page whose frame is one of `targets` into a
    /// frame of its own, charged to `budget`, and reads it from there, so
    /// that writes to `targets` do not change what is read. Fails with
    /// `NoMemory`, copying nothing that stays, when there is no frame or
    /// charge for a copy.
    pub fn snapshot<F: FnMut()>(
        &mut self,
        targets: impl Iterator<Item = u64>,
        budget: Option<&Arc<Budget>>,
        work: &mut Work<F>,
    ) -> Result<(), Error> {
        let mut targets: Vec<u64> = {
            let mut v = Vec::new();
            for t in targets {
                v.try_reserve(1).map_err(|_| Error::NoMemory)?;
                v.push(t);
            }
            v
        };
        if targets.is_empty() {
            return Ok(());
        }
        targets.sort_unstable();
        self.copies_charged = budget.cloned();
        for index in 0..self.frames.len() {
            let Some(phys) = self.frames[index] else {
                continue;
            };
            work.item();
            if targets.binary_search(&phys).is_err() {
                continue;
            }
            self.copies.try_reserve(1).map_err(|_| Error::NoMemory)?;
            if let Some(budget) = budget {
                budget.charge_kernel(PAGE_SIZE)?;
            }
            let Some(copy) = crate::frames::alloc_frame() else {
                if let Some(budget) = budget {
                    budget.release_kernel(PAGE_SIZE);
                }
                return Err(Error::NoMemory);
            };
            work.copy(PAGE_SIZE as usize);
            // SAFETY: the frame is pinned and the copy was just allocated;
            // nothing writes to either while no output is written yet.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    crate::arch::frame_address(phys) as *const u8,
                    crate::arch::frame_address(copy) as *mut u8,
                    PAGE_SIZE as usize,
                );
            }
            self.copies.push(copy);
            self.frames[index] = Some(copy);
        }
        Ok(())
    }

    /// Releases the frames, accounting for the work that takes.
    pub fn release<F: FnMut()>(mut self, work: &mut Work<F>) {
        if core::mem::take(&mut self.pinned) {
            self.mem.unpin(work);
        }
        self.free_copies();
    }

    fn free_copies(&mut self) {
        for copy in core::mem::take(&mut self.copies) {
            if let Some(f) = FRAMES.lock().as_mut() {
                f.free(copy, 1);
            }
            if let Some(budget) = &self.copies_charged {
                budget.release_kernel(PAGE_SIZE);
            }
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if core::mem::take(&mut self.pinned) {
            self.mem.unpin(&mut Work::new(|| {}));
        }
        self.free_copies();
        if let Some((budget, bytes)) = self.charged.take() {
            budget.release_kernel(bytes);
        }
    }
}

/// The next record ID. IDs only need to differ within an object.
static NEXT_RECORD: AtomicU64 = AtomicU64::new(1);

/// The record of a region in the memory object it maps: its entry in the
/// object's mappings, which goes when this is dropped. Boxed, so that the
/// region stays small.
pub struct Mapping(alloc::boxed::Box<Entry>);

struct Entry {
    mem: Arc<MemoryObject>,
    key: (u64, u64),
    writable: bool,
}

const _: () = assert!(
    size_of::<Entry>() + size_of::<((u64, u64), Weak<AddressSpace>)>() <= RECORD_ROOM,
    "a region's record exceeds the room its charge covers"
);

impl Mapping {
    /// Adds the entry `key` for `space` to the mappings of `mem`.
    fn add(
        mem: &Arc<MemoryObject>,
        key: (u64, u64),
        space: Weak<AddressSpace>,
        writable: bool,
    ) -> Result<Mapping, Error> {
        let entry = crate::fallible::try_box(Entry {
            mem: mem.clone(),
            key,
            writable,
        })?;
        let _claim = nodes::claim_insert()?;
        mem.mappings.lock().insert(key, space);
        if writable {
            mem.writable.fetch_add(1, Ordering::Relaxed);
        }
        // A mapping is a use of the object, whatever handle it was made
        // through.
        mem.use_begin();
        Ok(Mapping(entry))
    }
}

impl Record for Mapping {
    /// A record with this one's ID, so that a walk of the mappings that
    /// has not passed this record yet reaches the new one too.
    fn split(&self, start: u64) -> Result<Mapping, Error> {
        let Entry { mem, key, writable } = &*self.0;
        let space = mem.mappings.lock().get(key).cloned();
        let space = space.expect("a live record has an entry");
        Mapping::add(mem, (key.0, start), space, *writable)
    }

    fn set_writable(&mut self, writable: bool) {
        let entry = &mut *self.0;
        if entry.writable != writable {
            entry.writable = writable;
            if writable {
                entry.mem.writable.fetch_add(1, Ordering::Relaxed);
            } else {
                entry.mem.writable.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let Entry { mem, key, writable } = &*self.0;
        mem.mappings.lock().remove(key);
        if *writable {
            mem.writable.fetch_sub(1, Ordering::Relaxed);
        }
        mem.use_end();
    }
}

/// A `pager_supply` call under way that moves frames out of an object or
/// into it, counted in the object's `supplies` until dropped.
struct Supplying<'a>(&'a MemoryObject);

impl<'a> Supplying<'a> {
    /// Counts a supply for `mem` if `allowed` passes with the pages locked,
    /// where a seal or a revocation also looks at the count.
    fn begin(
        mem: &'a MemoryObject,
        allowed: impl FnOnce() -> Result<(), Error>,
    ) -> Result<Supplying<'a>, Error> {
        let _pages = mem.pages.lock();
        allowed()?;
        mem.supplies.fetch_add(1, Ordering::Relaxed);
        Ok(Supplying(mem))
    }
}

impl Drop for Supplying<'_> {
    fn drop(&mut self) {
        self.0.supplies.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What `move_group` joins to a move: a page of a member, whether it is
/// one lent to a paged object, and the room to record that.
struct Joined {
    mem: Arc<MemoryObject>,
    index: u64,
    lender: bool,
    claim: Option<nodes::Claim>,
}

/// Moves the lend group of the frame `phys`, whose clones use it too,
/// to a new frame with the same bytes, which is charged to the budget
/// that `phys` is charged to, whoever wrote. The clones keep `phys`. Returns
/// when the group moved, or when it is not for this call to move it, and
/// the caller looks at its page again.
pub(crate) fn move_group(phys: u64) -> Result<(), Error> {
    let charge = match crate::share::begin_move(phys) {
        crate::share::Begin::Gone => return Ok(()),
        crate::share::Begin::Busy => {
            let _ = crate::sched::sleep_until(crate::arch::monotonic_now() + 1_000_000);
            return Ok(());
        }
        crate::share::Begin::Ready(charge) => charge,
    };
    let moved = move_members(phys, &charge);
    crate::share::end_move(phys, matches!(moved, Ok(true)));
    if matches!(moved, Ok(false)) {
        // Something changed under the move: look again after a while.
        let _ = crate::sched::sleep_until(crate::arch::monotonic_now() + 1_000_000);
    }
    moved.map(|_| ())
}

/// Frees the frame `new` that a move allocated and did not use, and
/// gives its page back to `charge`.
fn free_new_frame(new: u64, charge: &Option<Arc<Budget>>) {
    if let Some(allocator) = FRAMES.lock().as_mut() {
        allocator.free(new, 1);
    }
    if let Some(budget) = charge {
        budget.release_pages(1);
    }
}

/// Ends the marks that a move that did not go through put up, and gives
/// back what it charged for the anchor.
fn unwind(joined: Vec<Joined>) {
    for j in joined {
        j.mem.unmark(j.index, j.index + 1);
        if !j.lender
            && let Some(budget) = &j.mem.budget
        {
            budget.release_kernel(PAGE_ENTRY_COST);
        }
    }
}

/// The work of `move_group` with the group's frame `phys` marked as
/// moving. Returns whether it moved.
fn move_members(phys: u64, charge: &Option<Arc<Budget>>) -> Result<bool, Error> {
    let members = crate::share::members(phys);
    let mut joined: Vec<Joined> = Vec::new();
    joined
        .try_reserve_exact(members.len())
        .map_err(|_| Error::NoMemory)?;
    // Pass 1: every member that still uses the frame is marked changing,
    // with what it needs to move recorded beforehand, so that the move
    // can no longer fail once pages are replaced.
    for (mem, index) in members {
        let pages = mem.pages.lock();
        let Some(lender) = mem.member_kind(&pages, index, phys) else {
            continue;
        };
        if mem.is_changing(index) {
            drop(pages);
            unwind(joined);
            return Ok(false);
        }
        // The anchor's new slot is charged as its old one was, and that
        // one's entry goes back when it ends.
        if !lender
            && let Some(budget) = &mem.budget
            && let Err(e) = budget.charge_kernel(PAGE_ENTRY_COST)
        {
            drop(pages);
            unwind(joined);
            return Err(e);
        }
        let prepared = mem.mark(index, index + 1).and_then(|()| match lender {
            true => nodes::claim_insert().map(Some).inspect_err(|_| {
                mem.unmark(index, index + 1);
            }),
            false => Ok(None),
        });
        drop(pages);
        match prepared {
            Ok(claim) => joined.push(Joined {
                mem,
                index,
                lender,
                claim,
            }),
            Err(e) => {
                if !lender && let Some(budget) = &mem.budget {
                    budget.release_kernel(PAGE_ENTRY_COST);
                }
                unwind(joined);
                return Err(e);
            }
        }
    }
    if joined.is_empty() {
        return Ok(false);
    }
    // The new frame, charged to the budget of the old one.
    if let Some(budget) = charge
        && let Err(e) = budget.charge_pages(1)
    {
        unwind(joined);
        return Err(e);
    }
    let Some(new) = crate::frames::alloc_frame() else {
        if let Some(budget) = charge {
            budget.release_pages(1);
        }
        unwind(joined);
        return Err(Error::NoMemory);
    };
    // SAFETY: the new frame was just allocated, and the old one stays
    // allocated while the group uses it: no store reaches it meanwhile, as
    // the writers wait for the move and no writable translation is left.
    unsafe {
        core::ptr::copy_nonoverlapping(
            crate::arch::frame_address(phys) as *const u8,
            crate::arch::frame_address(new) as *mut u8,
            PAGE_SIZE as usize,
        );
    }
    let lent = joined.iter().filter(|j| j.lender).count() as u64;
    let mut named: Vec<(Arc<MemoryObject>, u64)> = Vec::new();
    if named.try_reserve_exact(joined.len()).is_err() {
        free_new_frame(new, charge);
        unwind(joined);
        return Err(Error::NoMemory);
    }
    named.extend(joined.iter().map(|j| (j.mem.clone(), j.index)));
    if let Err(e) =
        crate::share::create_moved(new, charge.clone(), joined.len() as u64, lent, &named)
    {
        free_new_frame(new, charge);
        unwind(joined);
        return Err(e);
    }
    drop(named);
    // Pass 2: every member that still uses the frame gets the new one.
    let mut moved: Vec<(Arc<MemoryObject>, u64)> = Vec::new();
    moved
        .try_reserve_exact(joined.len())
        .expect("room was found for the same members");
    for j in joined {
        let mut pages = j.mem.pages.lock();
        let kind = match j.lender {
            true => crate::share::Kind::Lent,
            false => crate::share::Kind::Owner,
        };
        if pages.frame(j.index) != Some(phys) {
            // It left the frame since: evicted, discarded or shrunk.
            drop(pages);
            j.mem.unmark(j.index, j.index + 1);
            if !j.lender
                && let Some(budget) = &j.mem.budget
            {
                budget.release_kernel(PAGE_ENTRY_COST);
            }
            if crate::share::skip_moved(new, kind) {
                free_new_frame(new, charge);
            }
            continue;
        }
        let old = pages.replace(j.index, new, true);
        if let (Some(paged), Some(claim)) = (&j.mem.paged, j.claim) {
            let mut charges = paged.charges.lock();
            match charges.get_mut(&new) {
                Some(Charge::Lent(n)) => *n += 1,
                _ => {
                    charges.insert(new, Charge::Lent(1));
                }
            }
            drop(claim);
        }
        j.mem.leave(j.index, j.index + 1, old);
        drop(pages);
        moved.push((j.mem, j.index));
    }
    // Pass 3: no member's old translation of the frame is left before any
    // of them lets go of it.
    for (mem, index) in &moved {
        mem.unmap_in_steps(*index, *index + 1);
    }
    let did = !moved.is_empty();
    for (mem, index) in moved {
        if let Some(old) = mem.unmark(index, index + 1) {
            mem.end_old(old);
        }
    }
    Ok(did)
}

impl MemoryObject {
    /// Whether page `index` is a member of the lend group of `phys`: its
    /// frame is that one, not as a clone's, and, for a paged object, lent
    /// to it. Returns whether it is lent. The pages are locked.
    fn member_kind(&self, pages: &Pages, index: u64, phys: u64) -> Option<bool> {
        if pages.frame(index) != Some(phys) || !pages.is_shared(index) || pages.is_copy(index) {
            return None;
        }
        match &self.paged {
            None => Some(false),
            Some(paged) => {
                matches!(paged.charges.lock().get(&phys), Some(Charge::Lent(_))).then_some(true)
            }
        }
    }

    /// Ends the use of `old`, the frame that a page left, which kernel
    /// copies may still hold: while the pages are pinned it waits for the
    /// last unpin instead.
    fn end_old(&self, old: Frame) {
        loop {
            let mut pages = self.pages.lock();
            if !pages.pinned() {
                drop(pages);
                self.free([old]);
                return;
            }
            if pages.orphan(old).is_ok() {
                return;
            }
            drop(pages);
            self.wait_unpinned();
        }
    }
}

/// Runs `piece` on the pages from `first` to `end`, exclusive, in pieces
/// of at most `ITEM_STEP` pages, with a reschedule check between two
/// pieces. `piece` takes and releases the locks it needs, so that none is
/// held at a check. Stops at the first error.
fn in_pieces(
    first: u64,
    end: u64,
    mut piece: impl FnMut(u64, u64) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut from = first;
    while from < end {
        let to = end.min(from.saturating_add(ITEM_STEP as u64));
        piece(from, to)?;
        from = to;
        if from < end {
            crate::sched::preempt_point();
        }
    }
    Ok(())
}

impl Object for MemoryObject {
    fn header(&self) -> &Header {
        &self.header
    }
}

impl Drop for MemoryObject {
    fn drop(&mut self) {
        let frames = self.pages.lock().take_all();
        self.free(frames.into_values());
        if let Some(budget) = &self.budget {
            budget.release_kernel(self.charged);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::mock;
    use crate::port::Port;

    fn no_preempt() {}

    /// Fills `len` bytes from `offset` of `object` with `byte`.
    fn fill(object: &MemoryObject, offset: u64, len: u64, byte: u8) {
        let mut work = Work::new(no_preempt);
        object
            .write(offset, len, &mut work, |to| {
                to.fill(byte);
                Ok(())
            })
            .unwrap();
    }

    /// The `len` bytes from `offset` of `object`.
    fn bytes(object: &Arc<MemoryObject>, offset: u64, len: u64) -> Vec<u8> {
        let mut work = Work::new(no_preempt);
        let held = object
            .hold_for_read(offset, len, false, None, &mut work)
            .unwrap();
        let mut out = alloc::vec![0u8; len as usize];
        // Reads stay within one page.
        let mut done = 0;
        while done < len {
            let n = (PAGE_SIZE - (offset + done) % PAGE_SIZE).min(len - done);
            held.read(offset + done, &mut out[done as usize..(done + n) as usize]);
            done += n;
        }
        held.release(&mut work);
        out
    }

    #[test]
    fn clones_a_range_of_an_object() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let budget = Budget::root();
        let start = arena.free();
        let source = MemoryObject::new(None, 4 * PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, 0x11);
        // The second page stays zero and the third is written in part.
        fill(&source, 2 * PAGE_SIZE + 100, 50, 0x33);
        fill(&source, 3 * PAGE_SIZE, PAGE_SIZE, 0x44);
        let free = arena.free();
        let mut work = Work::new(no_preempt);
        let clone = source
            .clone_range(PAGE_SIZE, 3 * PAGE_SIZE, 0, &budget, &mut work)
            .unwrap();
        // No page was copied: the clone uses the frames of the source's.
        assert_eq!(arena.free(), free);
        let was = bytes(&source, PAGE_SIZE, 3 * PAGE_SIZE);
        assert_eq!(bytes(&clone, 0, 3 * PAGE_SIZE), was);
        assert!(
            was[PAGE_SIZE as usize + 99..][..51]
                .iter()
                .eq([0u8, 0x33].iter().chain([0x33u8; 49].iter()))
        );
        // Writes to either object stay in it.
        fill(&clone, 0, 10, 0x99);
        fill(&source, PAGE_SIZE + PAGE_SIZE, 10, 0x77);
        assert_eq!(bytes(&source, PAGE_SIZE, 10), [0u8; 10]);
        assert_eq!(bytes(&clone, 0, 10), [0x99u8; 10]);
        assert_eq!(bytes(&clone, PAGE_SIZE, 10), [0u8; 10]);
        drop((clone, source));
        assert_eq!(arena.free(), start);
    }

    #[test]
    fn clones_share_frames_until_one_side_writes() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let (owner_budget, caller) = (Budget::root(), Budget::root());
        let start = arena.free();
        let source = MemoryObject::new(Some(&owner_budget), 3 * PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, 0x11);
        fill(&source, 2 * PAGE_SIZE, PAGE_SIZE, 0x22);
        let (first, third) = {
            let pages = source.pages.lock();
            (pages.frame(0).unwrap(), pages.frame(2).unwrap())
        };
        let base = caller.usage();
        let mut work = Work::new(no_preempt);
        let clone = source
            .clone_range(0, 3 * PAGE_SIZE, 0, &caller, &mut work)
            .unwrap();
        // No frame was allocated and none was charged to the caller: it
        // pays for the clone's slots and the table's entries only.
        assert_eq!(arena.free(), start - 2);
        assert_eq!(caller.usage().1, base.1);
        assert!(caller.usage().0 > base.0 + 2 * PAGE_ENTRY_COST);
        {
            let pages = clone.pages.lock();
            assert_eq!((pages.frame(0), pages.frame(2)), (Some(first), Some(third)));
            // The page without a frame in the source has none in the clone.
            assert_eq!(pages.frame(1), None);
            assert!(pages.is_copy(0) && pages.is_copy(2));
        }
        // Each side's write gives it a frame of its own, once.
        fill(&source, 0, 4, 0xaa);
        assert_ne!(source.pages.lock().frame(0), Some(first));
        assert_eq!(bytes(&clone, 0, 4), alloc::vec![0x11; 4]);
        fill(&clone, 2 * PAGE_SIZE, 4, 0xbb);
        assert_ne!(clone.pages.lock().frame(2), Some(third));
        assert_eq!(bytes(&source, 2 * PAGE_SIZE, 4), alloc::vec![0x22; 4]);
        assert_eq!(arena.free(), start - 4);
        // The source's old frame is the clone's alone now, and written in
        // place; the frame of the clone's page is the source's.
        fill(&clone, 0, 4, 0xcc);
        assert_eq!(clone.pages.lock().frame(0), Some(first));
        fill(&source, 2 * PAGE_SIZE, 4, 0xdd);
        assert_eq!(source.pages.lock().frame(2), Some(third));
        assert_eq!(arena.free(), start - 4);
        drop((clone, source));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), caller.usage()), ((0, 0), base));
    }

    #[test]
    fn gives_back_the_tables_entries_when_the_clone_goes_first() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let (owner_budget, caller) = (Budget::root(), Budget::root());
        let start = arena.free();
        let source = MemoryObject::new(Some(&owner_budget), 3 * PAGE_SIZE, 0).unwrap();
        fill(&source, 0, 3 * PAGE_SIZE, 0x5a);
        let mut work = Work::new(no_preempt);
        let clone = source
            .clone_range(0, 3 * PAGE_SIZE, 0, &caller, &mut work)
            .unwrap();
        drop(clone);
        // The owner is the only use of its frames again, and the caller
        // has nothing left charged for them.
        assert_eq!(caller.usage(), (0, 0));
        fill(&source, 0, 4, 1);
        drop(source);
        assert_eq!((arena.free(), owner_budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn leaves_nothing_charged_when_the_slots_of_a_clone_do_not_fit() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let (owner_budget, caller) = (Budget::root(), Budget::root());
        let start = arena.free();
        let source = MemoryObject::new(Some(&owner_budget), 2 * PAGE_SIZE, 0).unwrap();
        fill(&source, 0, 2 * PAGE_SIZE, 0x5a);
        let mut work = Work::new(no_preempt);
        // Room for the clone object and the table's entries, but not for the
        // slots' entries.
        let room = size_of::<MemoryObject>() as u64 + 2 * crate::share::SHARE_COST + 1;
        caller.set_limits(room, u64::MAX);
        let refused = source.clone_range(0, 2 * PAGE_SIZE, 0, &caller, &mut work);
        assert_eq!(refused.err(), Some(Error::NoResources));
        assert_eq!(caller.usage(), (0, 0));
        fill(&source, 0, 4, 1);
        drop(source);
        assert_eq!((arena.free(), owner_budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn shrinks_a_source_whose_last_page_a_clone_shares() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let budget = Budget::root();
        let start = arena.free();
        let source = MemoryObject::new(Some(&budget), PAGE_SIZE, RESIZABLE).unwrap();
        fill(&source, 0, PAGE_SIZE, 0x5a);
        let mut work = Work::new(no_preempt);
        let clone = source
            .clone_range(0, PAGE_SIZE, 0, &budget, &mut work)
            .unwrap();
        source.set_size(100).unwrap();
        // The source's tail went to zero in a frame of its own, and the
        // clone still sees every byte.
        assert_eq!(bytes(&source, 0, 100), alloc::vec![0x5a; 100]);
        assert!(
            frame_head(source.pages.lock().frame(0).unwrap(), PAGE_SIZE as usize)[100..]
                .iter()
                .all(|&b| b == 0)
        );
        assert_eq!(
            bytes(&clone, 0, PAGE_SIZE),
            alloc::vec![0x5a; PAGE_SIZE as usize]
        );
        drop((clone, source));
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn is_unreferenced_until_a_handle_without_manage_exists() {
        use crate::object::handles::HandleTable;
        use crate::object::{Object, rights, signals};
        let _lock = mock::lock();
        let _arena = mock::Arena::new(8);
        let budget = Budget::root();
        let (object, port) = paged(&budget, 1);
        let unreferenced = || object.header().signals() & signals::UNREFERENCED != 0;
        assert!(unreferenced() && object.uses() == 0);
        let mut table = HandleTable::new();
        let all = rights::DUPLICATE | rights::READ | rights::MAP;
        // The handle that creates the object has MANAGE: no use.
        let manage = table
            .insert(object.clone(), all | rights::MANAGE | rights::WAIT, None)
            .unwrap();
        assert!(unreferenced());
        // A duplicate without MANAGE is a use, from the call on.
        let copy = table
            .duplicate(manage, rights::READ | rights::MAP, None)
            .unwrap();
        assert!(!unreferenced() && object.uses() == 1);
        let second = table.duplicate(copy, rights::SAME_RIGHTS, None);
        assert!(second.is_err(), "the copy cannot duplicate");
        // Narrowing a handle with MANAGE to one without makes a use before
        // the old one goes.
        let narrowed = table.replace(manage, rights::READ | rights::MAP).unwrap();
        assert!(!unreferenced() && object.uses() == 2);
        table.close(copy).unwrap();
        assert!(!unreferenced());
        table.close(narrowed).unwrap();
        assert!(unreferenced() && object.uses() == 0);
        drop((table, object, port));
    }

    #[test]
    fn counts_a_use_in_a_queued_entry_and_a_cloned_one() {
        use crate::object::handles::Entry;
        use crate::object::{Object, rights, signals};
        let _lock = mock::lock();
        let _arena = mock::Arena::new(8);
        let budget = Budget::root();
        let (object, port) = paged(&budget, 1);
        let entry = Entry::new(object.clone(), rights::READ);
        let cloned = entry.clone();
        assert_eq!(object.uses(), 2);
        assert_eq!(object.header().signals() & signals::UNREFERENCED, 0);
        drop(entry);
        assert_eq!(object.header().signals() & signals::UNREFERENCED, 0);
        drop(cloned);
        assert_ne!(object.header().signals() & signals::UNREFERENCED, 0);
        // An entry with MANAGE, or of an object that is not paged, is no use.
        let manage = Entry::new(object.clone(), rights::MANAGE | rights::READ);
        assert_eq!(object.uses(), 0);
        let plain = MemoryObject::new(None, PAGE_SIZE, 0).unwrap();
        let entry = Entry::new(plain.clone(), rights::READ);
        assert_eq!(plain.uses(), 0);
        drop((manage, entry, plain, object, port));
    }

    #[test]
    fn keeps_a_chain_of_clones_whichever_goes_first() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let budget = Budget::root();
        let start = arena.free();
        let mut work = Work::new(no_preempt);
        let source = MemoryObject::new(Some(&budget), 2 * PAGE_SIZE, 0).unwrap();
        fill(&source, 0, 2 * PAGE_SIZE, 0x5a);
        let middle = source
            .clone_range(0, 2 * PAGE_SIZE, 0, &budget, &mut work)
            .unwrap();
        let last = middle
            .clone_range(0, 2 * PAGE_SIZE, 0, &budget, &mut work)
            .unwrap();
        fill(&middle, 0, 8, 1);
        drop(middle);
        drop(source);
        // The last one still reads what the source had.
        assert_eq!(
            bytes(&last, 0, PAGE_SIZE),
            alloc::vec![0x5a; PAGE_SIZE as usize]
        );
        fill(&last, PAGE_SIZE, 8, 2);
        assert_eq!(bytes(&last, PAGE_SIZE, 8)[..], [2u8; 8]);
        drop(last);
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn copies_the_lent_pages_of_a_source_at_once() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let (owner_budget, caller) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (object, lent) = shared_page(&owner_budget);
        let mut work = Work::new(no_preempt);
        let clone = object
            .clone_range(0, PAGE_SIZE, 0, &caller, &mut work)
            .unwrap();
        // A lent page is one object with its lenders, so the clone got a
        // copy of its own, charged to the caller.
        let own = clone.pages.lock().frame(0).unwrap();
        assert_ne!(own, lent);
        assert!(!clone.pages.lock().is_copy(0));
        assert_eq!(bytes(&clone, 0, 8), alloc::vec![0x3c; 8]);
        assert_eq!(caller.usage().1, 1);
        assert!(!crate::share::has_copies(lent));
        drop(clone);
        crate::share::end(lent, crate::share::Kind::Lent);
        drop(object);
        assert_eq!(
            (arena.free(), owner_budget.usage(), caller.usage()),
            (start, (0, 0), (0, 0))
        );
    }

    #[test]
    fn takes_no_use_when_the_caller_has_no_room_for_the_entries() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(64);
        let (owner_budget, caller) = (Budget::root(), Budget::root());
        let start = arena.free();
        let source = MemoryObject::new(Some(&owner_budget), 2 * PAGE_SIZE, 0).unwrap();
        fill(&source, 0, 2 * PAGE_SIZE, 0x5a);
        let frames: Vec<u64> = (0..2)
            .map(|i| source.pages.lock().frame(i).unwrap())
            .collect();
        let mut work = Work::new(no_preempt);
        // Room for the clone object itself but not for what it shares.
        let room = size_of::<MemoryObject>() as u64 + 8;
        caller.set_limits(room, u64::MAX);
        let refused = source.clone_range(0, 2 * PAGE_SIZE, 0, &caller, &mut work);
        assert_eq!(refused.err(), Some(Error::NoResources));
        assert!(frames.iter().all(|&phys| !crate::share::has_copies(phys)));
        assert_eq!(caller.usage(), (0, 0));
        // Nothing is left of the clone: the source writes in place.
        fill(&source, 0, 4, 1);
        assert_eq!(source.pages.lock().frame(0), Some(frames[0]));
        drop(source);
        assert_eq!((arena.free(), owner_budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn keeps_the_page_of_a_lent_frame_until_its_last_use_ends() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let start = arena.free();
        let object = MemoryObject::new(Some(&budget), 2 * PAGE_SIZE, 0).unwrap();
        let after_new = budget.usage();
        fill(&object, 0, 2 * PAGE_SIZE, 0x5a);
        let phys = object.pages.lock().frame(0).unwrap();
        // One page was lent to a paged object, which holds a use.
        crate::share::lend(phys, Some(&budget), None).unwrap();
        assert_eq!(arena.free(), start - 2);
        let (kernel, pages) = budget.usage();
        assert_eq!(pages, 2);
        assert_eq!(kernel, after_new.0 + 2 * PAGE_ENTRY_COST);
        // The object goes: the unshared page is freed with its entry, the
        // lent one keeps its page and gives its entry back.
        drop(object);
        assert_eq!(arena.free(), start - 1);
        assert_eq!(budget.usage(), (0, 1));
        // The lend ends: the frame is freed and the page given back.
        crate::share::end(phys, crate::share::Kind::Lent);
        assert_eq!(arena.free(), start);
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn gives_everything_back_when_the_lend_ends_first() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let start = arena.free();
        let object = MemoryObject::new(Some(&budget), PAGE_SIZE, 0).unwrap();
        fill(&object, 0, PAGE_SIZE, 1);
        let phys = object.pages.lock().frame(0).unwrap();
        crate::share::lend(phys, Some(&budget), None).unwrap();
        crate::share::end(phys, crate::share::Kind::Lent);
        assert_eq!(arena.free(), start - 1);
        drop(object);
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    /// A paged object of `pages` pages charged to `budget`, whose requests
    /// go to a port of its own.
    fn paged(budget: &Arc<Budget>, pages: u64) -> (Arc<MemoryObject>, Arc<Port>) {
        let port = Port::new(budget).unwrap();
        let object = MemoryObject::new_paged(budget, 1, &port, 0, pages * PAGE_SIZE, 0).unwrap();
        (object, port)
    }

    #[test]
    fn lends_a_page_and_ends_the_lend_before_the_owner_goes() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (lender, port) = paged(&user, 2);
        let base = (owner.usage(), user.usage());
        let object = MemoryObject::new(Some(&owner), 2 * PAGE_SIZE, 0).unwrap();
        fill(&object, 0, 2 * PAGE_SIZE, 7);
        lender.lend(0, 2 * PAGE_SIZE, &object, 0).unwrap();
        // The lent frames stay charged to the owner and are used twice.
        assert_eq!(arena.free(), start - 2);
        drop(lender);
        // The paged object went: the frames and the owner's charges stay.
        assert_eq!(arena.free(), start - 2);
        assert_eq!(owner.usage().1, 2);
        drop(object);
        assert_eq!(arena.free(), start);
        assert_eq!((owner.usage(), user.usage().1), ((0, 0), base.1.1));
        drop(port);
    }

    #[test]
    fn lends_a_page_and_lets_the_owner_go_first() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (lender, port) = paged(&user, 1);
        let object = MemoryObject::new(Some(&owner), PAGE_SIZE, 0).unwrap();
        fill(&object, 0, PAGE_SIZE, 7);
        lender.lend(0, PAGE_SIZE, &object, 0).unwrap();
        drop(object);
        // The page stays with the lender, charged to the owner's budget,
        // and the owner's entry went with its slot.
        assert_eq!(arena.free(), start - 1);
        assert_eq!(owner.usage().1, 1);
        drop(lender);
        assert_eq!(arena.free(), start);
        assert_eq!(owner.usage(), (0, 0));
        drop(port);
    }

    #[test]
    fn supplies_a_frame_whose_lend_ended_and_frees_it_with_its_charges() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (lender, port) = paged(&user, 1);
        let (receiver, port2) = paged(&user, 1);
        let object = MemoryObject::new(Some(&owner), PAGE_SIZE, 0).unwrap();
        fill(&object, 0, PAGE_SIZE, 7);
        lender.lend(0, PAGE_SIZE, &object, 0).unwrap();
        drop(lender);
        // The lend is over, and the table still has an entry for the
        // frame, which the owner holds.
        receiver.supply(0, PAGE_SIZE, &object, 0).unwrap();
        drop(object);
        assert_eq!(arena.free(), start - 1);
        // The frame goes back with the page and the entry charged to the
        // owner's budget, which supplied it.
        drop(receiver);
        assert_eq!(arena.free(), start);
        assert_eq!(owner.usage(), (0, 0));
        drop((port, port2));
    }

    /// An object of one filled page, whose frame a lend shares, and that
    /// frame.
    fn shared_page(budget: &Arc<Budget>) -> (Arc<MemoryObject>, u64) {
        let object = MemoryObject::new(Some(budget), PAGE_SIZE, 0).unwrap();
        fill(&object, 0, PAGE_SIZE, 0x3c);
        let mut pages = object.pages.lock();
        let phys = pages.frame(0).unwrap();
        crate::share::lend(phys, Some(budget), None).unwrap();
        pages.mark_shared(0);
        drop(pages);
        (object, phys)
    }

    #[test]
    fn gives_a_shared_page_a_frame_of_its_own() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let start = arena.free();
        let (object, shared) = shared_page(&budget);
        let before = bytes(&object, 0, PAGE_SIZE);
        {
            let mut pages = object.pages.lock();
            object.break_slot(&mut pages, 0).unwrap();
            let copy = pages.frame(0).unwrap();
            assert_ne!(copy, shared);
            assert!(!pages.is_shared(0));
        }
        // The page is changing until its old translations are gone.
        assert!(object.is_changing(0));
        object.settle(0, 1);
        assert!(!object.is_changing(0));
        assert_eq!(bytes(&object, 0, PAGE_SIZE), before);
        // The lent frame stays for its user, with the page the lend holds.
        assert_eq!(arena.free(), start - 2);
        assert_eq!(budget.usage().1, 2);
        // Writes to the page no longer reach the shared frame.
        fill(&object, 0, 8, 0xff);
        assert!(crate::share::is_lent(shared));
        assert_eq!(frame_head(shared, 8), alloc::vec![0x3c; 8]);
        crate::share::end(shared, crate::share::Kind::Lent);
        assert_eq!(arena.free(), start - 1);
        assert_eq!(budget.usage().1, 1);
        drop(object);
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn leaves_a_shared_page_alone_without_room_in_the_budget() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let (object, shared) = shared_page(&budget);
        let start = arena.free();
        let used = budget.usage();
        budget.set_limits(u64::MAX, 1);
        let mut pages = object.pages.lock();
        assert_eq!(object.break_slot(&mut pages, 0), Err(Error::NoResources));
        assert_eq!((pages.frame(0), pages.is_shared(0)), (Some(shared), true));
        drop(pages);
        assert!(!object.is_changing(0));
        assert_eq!((arena.free(), budget.usage()), (start, used));
        budget.set_limits(u64::MAX, u64::MAX);
        crate::share::end(shared, crate::share::Kind::Lent);
    }

    #[test]
    fn leaves_a_shared_page_alone_without_a_frame() {
        let _lock = mock::lock();
        // One frame, which the object's page takes.
        let arena = mock::Arena::new(2);
        let budget = Budget::root();
        let (object, shared) = shared_page(&budget);
        assert_eq!(arena.free(), 0);
        let used = budget.usage();
        let mut pages = object.pages.lock();
        assert_eq!(object.break_slot(&mut pages, 0), Err(Error::NoMemory));
        assert_eq!(pages.frame(0), Some(shared));
        drop(pages);
        assert!(!object.is_changing(0));
        assert_eq!(budget.usage(), used);
        crate::share::end(shared, crate::share::Kind::Lent);
    }

    #[test]
    fn keeps_the_old_frame_of_a_pinned_page_until_the_last_unpin() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let start = arena.free();
        let (object, shared) = shared_page(&budget);
        object.pages.lock().pin();
        {
            let mut pages = object.pages.lock();
            object.break_slot(&mut pages, 0).unwrap();
        }
        object.settle(0, 1);
        // The old frame is still this slot's use, which the unpin ends.
        assert_eq!(arena.free(), start - 2);
        let mut work = Work::new(no_preempt);
        object.unpin(&mut work);
        crate::share::end(shared, crate::share::Kind::Lent);
        assert_eq!(arena.free(), start - 1);
        drop(object);
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    /// An object of one filled page whose frame a clone's slot shares,
    /// which `share::copy` records as the clone's creation will, and that
    /// frame.
    fn copied_page(budget: &Arc<Budget>) -> (Arc<MemoryObject>, u64) {
        let object = MemoryObject::new(Some(budget), PAGE_SIZE, 0).unwrap();
        fill(&object, 0, PAGE_SIZE, 0x3c);
        let mut pages = object.pages.lock();
        let phys = pages.frame(0).unwrap();
        crate::share::copy(phys, Some(budget), None).unwrap();
        pages.mark_shared(0);
        drop(pages);
        (object, phys)
    }

    /// The first `n` bytes of the frame `phys`.
    fn frame_head(phys: u64, n: usize) -> Vec<u8> {
        // SAFETY: the frame is allocated in the arena.
        let frame = unsafe { frame_bytes(phys) };
        frame[..n].to_vec()
    }

    #[test]
    fn writes_a_page_that_a_clone_shares_to_a_frame_of_its_own() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let start = arena.free();
        let (object, shared) = copied_page(&budget);
        fill(&object, 10, 20, 0x99);
        let mine = object.pages.lock().frame(0).unwrap();
        assert_ne!(mine, shared);
        // The clone's frame keeps the bytes it had.
        assert_eq!(frame_head(shared, 40), alloc::vec![0x3c; 40]);
        let own = frame_head(mine, 40);
        assert!(own[..10].iter().all(|&b| b == 0x3c));
        assert!(own[10..30].iter().all(|&b| b == 0x99));
        assert!(own[30..].iter().all(|&b| b == 0x3c));
        // A second write stays in the frame the object has.
        fill(&object, 0, 4, 1);
        assert_eq!(object.pages.lock().frame(0), Some(mine));
        crate::share::end(shared, crate::share::Kind::Copy);
        drop(object);
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn writes_a_lent_page_in_place() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let (object, shared) = shared_page(&budget);
        let start = arena.free();
        fill(&object, 0, 8, 0x11);
        // The lenders and the owner are one object, which sees the write.
        assert_eq!(object.pages.lock().frame(0), Some(shared));
        assert_eq!(frame_head(shared, 8), alloc::vec![0x11; 8]);
        assert_eq!(arena.free(), start);
        crate::share::end(shared, crate::share::Kind::Lent);
    }

    #[test]
    fn zeroes_the_tail_of_a_page_a_clone_shares_in_a_frame_of_its_own() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let start = arena.free();
        let object = MemoryObject::new(Some(&budget), PAGE_SIZE, RESIZABLE).unwrap();
        fill(&object, 0, PAGE_SIZE, 0x3c);
        let shared = {
            let mut pages = object.pages.lock();
            let phys = pages.frame(0).unwrap();
            crate::share::copy(phys, Some(&budget), None).unwrap();
            pages.mark_shared(0);
            phys
        };
        object.set_size(100).unwrap();
        let mine = object.pages.lock().frame(0).unwrap();
        assert_ne!(mine, shared);
        // The clone still sees every byte, and the object only the head.
        assert!(
            frame_head(shared, PAGE_SIZE as usize)
                .iter()
                .all(|&b| b == 0x3c)
        );
        let own = frame_head(mine, PAGE_SIZE as usize);
        assert!(own[..100].iter().all(|&b| b == 0x3c));
        assert!(own[100..].iter().all(|&b| b == 0));
        crate::share::end(shared, crate::share::Kind::Copy);
        drop(object);
        assert_eq!((arena.free(), budget.usage()), (start, (0, 0)));
    }

    #[test]
    fn leaves_a_shared_page_alone_when_the_shrink_is_refused() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let budget = Budget::root();
        let object = MemoryObject::new(Some(&budget), PAGE_SIZE, RESIZABLE).unwrap();
        fill(&object, 0, PAGE_SIZE, 0x3c);
        let shared = {
            let mut pages = object.pages.lock();
            let phys = pages.frame(0).unwrap();
            crate::share::copy(phys, Some(&budget), None).unwrap();
            pages.mark_shared(0);
            phys
        };
        object.seal(SEAL_SHRINK).unwrap();
        let (free, used) = (arena.free(), budget.usage());
        assert_eq!(object.set_size(100), Err(Error::AccessDenied));
        assert_eq!(object.pages.lock().frame(0), Some(shared));
        assert_eq!((arena.free(), budget.usage()), (free, used));
        crate::share::end(shared, crate::share::Kind::Copy);
    }

    #[test]
    fn zeroes_the_tail_of_an_unshared_page_in_place() {
        let _lock = mock::lock();
        let _arena = mock::Arena::new(16);
        let budget = Budget::root();
        let object = MemoryObject::new(Some(&budget), PAGE_SIZE, RESIZABLE).unwrap();
        fill(&object, 0, PAGE_SIZE, 0x3c);
        let frame = object.pages.lock().frame(0).unwrap();
        object.set_size(100).unwrap();
        assert_eq!(object.pages.lock().frame(0), Some(frame));
        assert!(
            frame_head(frame, PAGE_SIZE as usize)[100..]
                .iter()
                .all(|&b| b == 0)
        );
    }

    /// A slot of a clone of `owner`'s page 0, which a clone made by
    /// `mem_clone` has: the table counts it, the clone's budget pays for
    /// the slot's entry and, when the entry is new, for the table's.
    fn clone_of(owner: &Arc<MemoryObject>, budget: &Arc<Budget>) -> Arc<MemoryObject> {
        let clone = MemoryObject::new(Some(budget), PAGE_SIZE, 0).unwrap();
        let mut pages = owner.pages.lock();
        let phys = pages.frame(0).unwrap();
        crate::share::copy(phys, owner.budget.as_ref(), Some(budget)).unwrap();
        pages.mark_shared(0);
        drop(pages);
        budget.charge_kernel(PAGE_ENTRY_COST).unwrap();
        clone
            .pages
            .lock()
            .insert_copy(0, phys, nodes::claim_insert().unwrap());
        clone
    }

    #[test]
    fn ends_the_use_of_a_clone_as_a_clone() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, clone_budget) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let base = clone_budget.usage();
        let clone = clone_of(&owner, &clone_budget);
        assert!(crate::share::has_copies(shared));
        // The clone's budget pays for its slot and the table's entry.
        assert!(clone_budget.usage().0 > base.0 + PAGE_ENTRY_COST);
        assert_eq!(clone_budget.usage().1, base.1);
        drop(clone);
        // Once the clone is gone the owner writes in place again.
        assert!(!crate::share::has_copies(shared));
        // The slot's entry went back, and so did the table's, as the owner
        // is the frame's only use.
        assert_eq!(clone_budget.usage(), base);
        fill(&owner, 0, 4, 1);
        assert_eq!(owner.pages.lock().frame(0), Some(shared));
        drop(owner);
        assert_eq!((arena.free(), owner_budget.usage()), (start, (0, 0)));
        assert_eq!(clone_budget.usage(), base);
    }

    #[test]
    fn writes_a_clones_page_in_place_once_it_is_the_only_use() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, clone_budget) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, &clone_budget);
        // While the owner holds the frame, a write to the clone copies.
        fill(&clone, 0, 4, 9);
        let own = clone.pages.lock().frame(0).unwrap();
        assert_ne!(own, shared);
        assert_eq!(frame_head(shared, 4), alloc::vec![0x3c; 4]);
        // The owner is the only use left of the shared frame now.
        fill(&owner, 0, 4, 2);
        assert_eq!(owner.pages.lock().frame(0), Some(shared));
        drop((clone, owner));
        assert_eq!(arena.free(), start);
        assert_eq!(
            (owner_budget.usage(), clone_budget.usage()),
            ((0, 0), (0, 0))
        );
    }

    #[test]
    fn writes_a_clones_page_in_place_when_the_owner_is_gone() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, clone_budget) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, &clone_budget);
        drop(owner);
        // The clone is the only use of the frame now, still charged to the
        // owner's budget.
        assert_eq!(crate::share::uses(shared), 1);
        assert_eq!(owner_budget.usage().1, 1);
        fill(&clone, 0, 4, 5);
        assert_eq!(clone.pages.lock().frame(0), Some(shared));
        assert_eq!(frame_head(shared, 4), alloc::vec![5; 4]);
        drop(clone);
        assert_eq!(arena.free(), start);
        assert_eq!(
            (owner_budget.usage(), clone_budget.usage()),
            ((0, 0), (0, 0))
        );
    }

    #[test]
    fn moves_the_frame_of_a_clone_that_is_its_only_use_to_a_paged_object_as_it_is() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, &user);
        drop(owner);
        let (receiver, port) = paged(&user, 1);
        receiver.supply(0, PAGE_SIZE, &clone, 0).unwrap();
        // Nothing else used the frame, so it moved as it was, still counted
        // as a clone's.
        assert_eq!(receiver.pages.lock().frame(0), Some(shared));
        assert!(receiver.pages.lock().is_copy(0));
        drop((receiver, clone, port));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn copies_the_frame_of_a_clone_when_it_goes_to_a_paged_object() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, &user);
        let (receiver, port) = paged(&user, 1);
        receiver.supply(0, PAGE_SIZE, &clone, 0).unwrap();
        // The paged object got a copy and the owner's frame is untouched.
        let got = receiver.pages.lock().frame(0).unwrap();
        assert_ne!(got, shared);
        assert!(!crate::share::has_copies(shared));
        assert_eq!(frame_head(got, 4), alloc::vec![0x3c; 4]);
        drop((receiver, clone, port));
        fill(&owner, 0, 4, 1);
        assert_eq!(owner.pages.lock().frame(0), Some(shared));
        drop(owner);
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn gives_a_page_a_frame_of_its_own_before_it_is_lent_while_a_clone_shares_it() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, &user);
        let (lender, port) = paged(&user, 1);
        lender.lend(0, PAGE_SIZE, &owner, 0).unwrap();
        // What is lent is the owner's own frame, and the clone keeps the
        // old one, which no write to the lent page can reach.
        let lent = owner.pages.lock().frame(0).unwrap();
        assert_ne!(lent, shared);
        assert!(crate::share::is_lent(lent) && !crate::share::is_lent(shared));
        assert_eq!(lender.pages.lock().frame(0), Some(lent));
        drop((lender, clone, port, owner));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn refuses_a_clone_outside_the_object() {
        let _lock = mock::lock();
        let _arena = mock::Arena::new(16);
        let budget = Budget::root();
        let source = MemoryObject::new(None, 2 * PAGE_SIZE, 0).unwrap();
        let mut work = Work::new(no_preempt);
        let past = source.clone_range(PAGE_SIZE, 2 * PAGE_SIZE, 0, &budget, &mut work);
        assert_eq!(past.err(), Some(Error::OutOfRange));
    }

    #[test]
    fn a_clone_shares_the_supplied_frame_of_a_paged_object_until_either_is_written() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let source = MemoryObject::new(Some(&supplier), PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, 0x3c);
        let (origin, port) = paged(&user, 1);
        origin.supply(0, PAGE_SIZE, &source, 0).unwrap();
        drop(source);
        let frame = origin.pages.lock().frame(0).unwrap();
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        // Nothing was copied, and the page is still charged to the supplier.
        assert_eq!(clone.pages.lock().frame(0), Some(frame));
        assert!(crate::share::has_copies(frame));
        assert_eq!(supplier.usage().1, 1);
        // A write through the paged object gives it a frame of its own,
        // charged to the supplier as the first was; the clone keeps the
        // bytes it had.
        fill(&origin, 0, 4, 0x77);
        let own = origin.pages.lock().frame(0).unwrap();
        assert_ne!(own, frame);
        assert_eq!(clone.pages.lock().frame(0), Some(frame));
        assert_eq!(frame_head(frame, 4), alloc::vec![0x3c; 4]);
        assert_eq!(frame_head(own, 4), alloc::vec![0x77; 4]);
        assert_eq!(supplier.usage().1, 2);
        // The frame goes with the clone, which was the last to use it.
        drop(clone);
        assert_eq!(supplier.usage().1, 1);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn a_write_through_a_clone_of_a_paged_object_leaves_the_object_alone() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let source = MemoryObject::new(Some(&supplier), PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, 0x3c);
        let (origin, port) = paged(&user, 1);
        origin.supply(0, PAGE_SIZE, &source, 0).unwrap();
        drop(source);
        let frame = origin.pages.lock().frame(0).unwrap();
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        fill(&clone, 0, 4, 0x55);
        // The object keeps its frame, which no clone uses any more.
        assert_eq!(origin.pages.lock().frame(0), Some(frame));
        assert!(!crate::share::has_copies(frame));
        assert_eq!(frame_head(frame, 4), alloc::vec![0x3c; 4]);
        let copy = clone.pages.lock().frame(0).unwrap();
        assert_eq!(frame_head(copy, 4), alloc::vec![0x55; 4]);
        drop(clone);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage().1), ((0, 0), 0));
    }

    /// A paged object whose one page, filled with `byte`, was supplied from
    /// an object charged to `supplier`.
    fn supplied(
        supplier: &Arc<Budget>,
        user: &Arc<Budget>,
        byte: u8,
    ) -> (Arc<MemoryObject>, Arc<Port>) {
        let source = MemoryObject::new(Some(supplier), PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, byte);
        let (origin, port) = paged(user, 1);
        origin.supply(0, PAGE_SIZE, &source, 0).unwrap();
        (origin, port)
    }

    #[test]
    fn a_paged_object_keeps_its_frame_when_the_clone_that_shared_it_goes_first() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (origin, port) = supplied(&supplier, &user, 0x3c);
        let frame = origin.pages.lock().frame(0).unwrap();
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        drop(clone);
        // No clone is left, so a write needs no copy.
        fill(&origin, 0, 4, 0x77);
        assert_eq!(origin.pages.lock().frame(0), Some(frame));
        assert_eq!(supplier.usage().1, 1);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn a_clone_keeps_the_frame_of_a_page_that_the_paged_object_evicts() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (origin, port) = supplied(&supplier, &user, 0x3c);
        let frame = origin.pages.lock().frame(0).unwrap();
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        origin.pager_op(OP_EVICT, 0, PAGE_SIZE).unwrap();
        assert_eq!(origin.pages.lock().frame(0), None);
        // The frame stays charged to the supplier while the clone uses it.
        assert_eq!(clone.pages.lock().frame(0), Some(frame));
        assert_eq!(frame_head(frame, 4), alloc::vec![0x3c; 4]);
        assert_eq!(supplier.usage().1, 1);
        drop(clone);
        assert_eq!(supplier.usage().1, 0);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn a_write_to_a_shared_page_of_a_paged_object_fails_when_the_supplier_has_no_room() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let root = Budget::root();
        let user = Budget::root();
        // Room for the page that is supplied, and for no copy of it.
        let supplier = Budget::child(&root, u64::MAX, 1, None).unwrap();
        let start = arena.free();
        let (origin, port) = supplied(&supplier, &user, 0x3c);
        let frame = origin.pages.lock().frame(0).unwrap();
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        let mut work = Work::new(no_preempt);
        let written = origin.write(0, 4, &mut work, |to| {
            to.fill(0x77);
            Ok(())
        });
        assert_eq!(written, Err(Error::NoResources));
        // Nothing changed: the object and the clone still use the frame.
        assert_eq!(origin.pages.lock().frame(0), Some(frame));
        assert_eq!(frame_head(frame, 4), alloc::vec![0x3c; 4]);
        assert_eq!(supplier.usage().1, 1);
        drop((clone, origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage().1), ((0, 0), 0));
    }

    /// A paged object of two pages whose first page was supplied with
    /// `byte` from an object charged to `supplier`, the second absent.
    fn half_supplied(
        supplier: &Arc<Budget>,
        user: &Arc<Budget>,
        byte: u8,
    ) -> (Arc<MemoryObject>, Arc<Port>) {
        let source = MemoryObject::new(Some(supplier), PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, byte);
        let (origin, port) = paged(user, 2);
        origin.supply(0, PAGE_SIZE, &source, 0).unwrap();
        (origin, port)
    }

    /// Supplies the second page of `origin`, as its pager does once the
    /// page is asked for.
    fn supply_second(origin: &MemoryObject, supplier: &Arc<Budget>, byte: u8) {
        let source = MemoryObject::new(Some(supplier), PAGE_SIZE, 0).unwrap();
        fill(&source, 0, PAGE_SIZE, byte);
        origin.supply(PAGE_SIZE, PAGE_SIZE, &source, 0).unwrap();
    }

    #[test]
    fn a_clone_leaves_the_pages_absent_from_its_origin_pending_until_it_uses_them() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (origin, port) = half_supplied(&supplier, &user, 0x3c);
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, 2 * PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        // The first page is shared, and the second is neither copied nor
        // requested: it is pending, and keeps the origin in use.
        assert!(clone.pages.lock().frame(0).is_some());
        assert_eq!(clone.pages.lock().frame(1), None);
        assert!(MemoryObject::is_pending(&clone.pages.lock(), 1));
        assert!(!MemoryObject::is_pending(&clone.pages.lock(), 0));
        assert_eq!(origin.uses(), 1);
        // The page is supplied meanwhile, and the clone's first use of it
        // takes what the origin has then.
        supply_second(&origin, &supplier, 0x55);
        let frame = origin.pages.lock().frame(1).unwrap();
        clone.resolve_pending(1).unwrap();
        assert_eq!(clone.pages.lock().frame(1), Some(frame));
        assert!(clone.pages.lock().is_copy(1));
        assert!(clone.pages.lock().pending.is_none());
        assert_eq!(origin.uses(), 0);
        assert_eq!(frame_head(frame, 4), alloc::vec![0x55; 4]);
        // A write through the origin then gives it a frame of its own.
        fill(&origin, PAGE_SIZE, 4, 0x77);
        assert_ne!(origin.pages.lock().frame(1), Some(frame));
        assert_eq!(frame_head(frame, 4), alloc::vec![0x55; 4]);
        drop(clone);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage()), ((0, 0), (0, 0)));
    }

    #[test]
    fn pending_pages_go_with_the_clone_or_become_zero_when_decommitted() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (origin, port) = half_supplied(&supplier, &user, 0x3c);
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, 2 * PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        // The clone of a clone takes over the pending page, which the
        // origin stays in use for.
        let again = clone
            .clone_range(PAGE_SIZE, PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        assert!(MemoryObject::is_pending(&again.pages.lock(), 0));
        assert_eq!(origin.uses(), 2);
        // Decommitting the pending page makes it read as zero, and ends it.
        clone.decommit(PAGE_SIZE, PAGE_SIZE).unwrap();
        assert!(clone.pages.lock().pending.is_none());
        assert_eq!(origin.uses(), 1);
        drop(again);
        assert_eq!(origin.uses(), 0);
        drop(clone);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage()), ((0, 0), (0, 0)));
    }

    #[test]
    fn a_shrunk_clone_forgets_the_pending_pages_it_lost() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (supplier, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (origin, port) = half_supplied(&supplier, &user, 0x3c);
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, 2 * PAGE_SIZE, RESIZABLE, &user, &mut work)
            .unwrap();
        assert_eq!(origin.uses(), 1);
        clone.set_size(PAGE_SIZE).unwrap();
        assert!(clone.pages.lock().pending.is_none());
        assert_eq!(origin.uses(), 0);
        drop(clone);
        drop((origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!((supplier.usage(), user.usage()), ((0, 0), (0, 0)));
    }

    #[test]
    fn decommitting_the_middle_of_the_pending_pages_leaves_those_around_it_pending() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let user = Budget::root();
        let start = arena.free();
        let (origin, port) = paged(&user, 4);
        let mut work = Work::new(no_preempt);
        let clone = origin
            .clone_range(0, 4 * PAGE_SIZE, 0, &user, &mut work)
            .unwrap();
        clone.decommit(PAGE_SIZE, 2 * PAGE_SIZE).unwrap();
        let pages = clone.pages.lock();
        let pending = |i| MemoryObject::is_pending(&pages, i);
        assert_eq!(
            [pending(0), pending(1), pending(2), pending(3)],
            [true, false, false, true]
        );
        drop(pages);
        // The origin stays in use until the last pending page is gone.
        assert_eq!(origin.uses(), 1);
        clone.decommit(0, PAGE_SIZE).unwrap();
        assert_eq!(origin.uses(), 1);
        clone.decommit(3 * PAGE_SIZE, PAGE_SIZE).unwrap();
        assert_eq!(origin.uses(), 0);
        drop((clone, origin, port));
        assert_eq!(arena.free(), start);
        assert_eq!(user.usage(), (0, 0));
    }

    #[test]
    fn lends_a_frame_of_its_own_from_the_slot_of_a_clone_that_is_the_only_user() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let shared = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, &user);
        drop(owner);
        let (lender, port) = paged(&user, 1);
        lender.lend(0, PAGE_SIZE, &clone, 0).unwrap();
        // The clone is not the owner of the frame it had, so what is lent
        // is a frame that is its own, with the bytes it had.
        let lent = clone.pages.lock().frame(0).unwrap();
        assert_ne!(lent, shared);
        assert!(!clone.pages.lock().is_copy(0));
        assert!(crate::share::is_lent(lent) && !crate::share::has_copies(lent));
        assert_eq!(lender.pages.lock().frame(0), Some(lent));
        assert_eq!(frame_head(lent, 4), alloc::vec![0x3c; 4]);
        drop((lender, clone, port));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    fn member_pages(phys: u64) -> Vec<(u64, u64)> {
        let mut found: Vec<(u64, u64)> = crate::share::members(phys)
            .into_iter()
            .map(|(mem, index)| (mem.koid(), index))
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn a_lend_records_the_source_and_the_lender_as_the_members_of_the_frame() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), 2 * PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, 2 * PAGE_SIZE, 0x3c);
        let phys = owner.pages.lock().frame(1).unwrap();
        let (first, port) = paged(&user, 2);
        let (second, port2) = paged(&user, 2);
        first.lend(PAGE_SIZE, PAGE_SIZE, &owner, PAGE_SIZE).unwrap();
        second.lend(0, PAGE_SIZE, &owner, PAGE_SIZE).unwrap();
        let mut expected = alloc::vec![(owner.koid(), 1), (first.koid(), 1), (second.koid(), 0)];
        expected.sort_unstable();
        assert_eq!(member_pages(phys), expected);
        // Lending it again to the same page adds nothing.
        first.pager_op(OP_EVICT, PAGE_SIZE, PAGE_SIZE).unwrap();
        first.lend(PAGE_SIZE, PAGE_SIZE, &owner, PAGE_SIZE).unwrap();
        assert_eq!(member_pages(phys), expected);
        // An object that uses the frame no more is forgotten by its pages.
        first.pager_op(OP_EVICT, PAGE_SIZE, PAGE_SIZE).unwrap();
        let mut left = alloc::vec![(owner.koid(), 1), (second.koid(), 0)];
        left.sort_unstable();
        assert_eq!(member_pages(phys), left);
        // The last lender going leaves the source alone with its frame,
        // which is not shared and has no group.
        drop((second, port2));
        assert!(member_pages(phys).is_empty());
        drop((first, port, owner));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    /// An owner with a page, which is lent to a paged object and shared
    /// with a clone, as a clone of the lent page makes it.
    fn lent_and_cloned(
        owner_budget: &Arc<Budget>,
        user: &Arc<Budget>,
    ) -> (
        Arc<MemoryObject>,
        Arc<MemoryObject>,
        Arc<Port>,
        Arc<MemoryObject>,
        u64,
    ) {
        let owner = MemoryObject::new(Some(owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let (lender, port) = paged(user, 1);
        lender.lend(0, PAGE_SIZE, &owner, 0).unwrap();
        let old = owner.pages.lock().frame(0).unwrap();
        let clone = clone_of(&owner, user);
        (owner, lender, port, clone, old)
    }

    #[test]
    fn a_write_through_the_source_moves_the_lend_group_and_leaves_the_clone_its_frame() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (owner, lender, port, clone, old) = lent_and_cloned(&owner_budget, &user);
        let used = owner_budget.usage().1;
        fill(&owner, 0, 4, 0x77);
        // The source and the lender use one new frame, charged to the
        // owner as the old one was; the clone keeps the old one.
        let new = owner.pages.lock().frame(0).unwrap();
        assert_ne!(new, old);
        assert_eq!(lender.pages.lock().frame(0), Some(new));
        assert_eq!(clone.pages.lock().frame(0), Some(old));
        assert_eq!(frame_head(new, 4), alloc::vec![0x77; 4]);
        assert_eq!(frame_head(old, 4), alloc::vec![0x3c; 4]);
        assert_eq!(owner_budget.usage().1, used + 1);
        assert_eq!(arena.free(), start - 2);
        assert!(crate::share::is_lent(new) && !crate::share::is_lent(old));
        // A write through the lender is now seen by the source.
        fill(&lender, 0, 4, 0x55);
        assert_eq!(frame_head(new, 4), alloc::vec![0x55; 4]);
        assert_eq!(owner.pages.lock().frame(0), Some(new));
        drop(clone);
        assert_eq!(owner_budget.usage().1, used);
        drop((lender, port, owner));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn a_write_through_a_lender_moves_the_lend_group_too() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let (owner, lender, port, clone, old) = lent_and_cloned(&owner_budget, &user);
        let used = owner_budget.usage().1;
        fill(&lender, 0, 4, 0x66);
        let new = lender.pages.lock().frame(0).unwrap();
        assert_ne!(new, old);
        assert_eq!(owner.pages.lock().frame(0), Some(new));
        assert_eq!(clone.pages.lock().frame(0), Some(old));
        assert_eq!(frame_head(new, 4), alloc::vec![0x66; 4]);
        assert_eq!(frame_head(old, 4), alloc::vec![0x3c; 4]);
        // The owner's budget paid for the new frame, not the user's.
        assert_eq!(owner_budget.usage().1, used + 1);
        drop((clone, lender, port, owner));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn a_write_that_moves_the_group_fails_when_the_owner_has_no_room() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (root, user) = (Budget::root(), Budget::root());
        let owner_budget = Budget::child(&root, u64::MAX, 1, None).unwrap();
        let start = arena.free();
        let (owner, lender, port, clone, old) = lent_and_cloned(&owner_budget, &user);
        let mut work = Work::new(no_preempt);
        let written = lender.write(0, 4, &mut work, |to| {
            to.fill(0x66);
            Ok(())
        });
        assert_eq!(written, Err(Error::NoResources));
        // Nothing moved, and nothing stays marked or charged.
        assert_eq!(owner.pages.lock().frame(0), Some(old));
        assert_eq!(lender.pages.lock().frame(0), Some(old));
        assert_eq!(clone.pages.lock().frame(0), Some(old));
        assert!(!owner.is_changing(0) && !lender.is_changing(0));
        assert_eq!(frame_head(old, 4), alloc::vec![0x3c; 4]);
        assert_eq!(owner_budget.usage().1, 1);
        // The group can move once there is room.
        owner_budget.set_limits(u64::MAX, 2);
        fill(&lender, 0, 4, 0x66);
        assert_ne!(lender.pages.lock().frame(0), Some(old));
        drop((clone, lender, port, owner));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage().1), ((0, 0), 0));
    }

    #[test]
    fn a_clone_waits_for_a_kernel_write_into_a_frame_of_a_lend_group() {
        let _lock = mock::lock();
        let arena = mock::Arena::new(16);
        let (owner_budget, user) = (Budget::root(), Budget::root());
        let start = arena.free();
        let owner = MemoryObject::new(Some(&owner_budget), PAGE_SIZE, 0).unwrap();
        fill(&owner, 0, PAGE_SIZE, 0x3c);
        let (lender, port) = paged(&user, 1);
        lender.lend(0, PAGE_SIZE, &owner, 0).unwrap();
        let phys = owner.pages.lock().frame(0).unwrap();
        // A write that is counted keeps a clone from taking the frame.
        assert_eq!(crate::share::hold(phys), crate::share::Hold::Counted);
        assert_eq!(
            crate::share::copy(phys, owner.budget.as_ref(), Some(&user)),
            Err(Error::BadState)
        );
        crate::share::unhold(phys);
        // Once it ended a clone takes it, and a write that begins then
        // moves the group first.
        crate::share::copy(phys, owner.budget.as_ref(), Some(&user)).unwrap();
        assert_eq!(crate::share::hold(phys), crate::share::Hold::Moved);
        crate::share::end(phys, crate::share::Kind::Copy);
        drop((lender, port, owner));
        assert_eq!(arena.free(), start);
        assert_eq!((owner_budget.usage(), user.usage()), ((0, 0), (0, 0)));
    }
}
