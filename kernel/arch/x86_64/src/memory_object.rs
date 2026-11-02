//! Anonymous memory objects: their frames and the copies to and from them.

use crate::aspace::{self, Access, AddressSpace, Fault, USER_END, USER_START};
use crate::memory::FRAMES;
use crate::paging::Policy;
use crate::port::Port;
use crate::publish::{ITEM_STEP, Work};
use crate::space;

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use kernel_core::nodes::{self, Map};
use kernel_core::object::budget::{Budget, Scratch};
use kernel_core::object::memory::{
    INFO_MEMORY_SIZE, KIND_ANONYMOUS, KIND_PAGED, KIND_PHYSICAL, PAGE_ENTRY_COST, PAGE_SIZE, Pages,
    RESIZABLE, SEAL_GROW, SEAL_SEAL, SEAL_SHRINK, SEAL_WRITE, SEALS, TRACK_DIRTY,
};
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::paging::{Absent, Paging};
use kernel_core::regions::{PERM_EXECUTE, PERM_WRITE, RECORD_ROOM, Record};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The largest memory object: the size of the user address range.
const MAX_SIZE: u64 = USER_END - USER_START;

/// The most bytes copied while the pages are locked. Copies to and from
/// user memory happen in between, without the lock, as they can fault.
const CHUNK: usize = 256;

pub struct MemoryObject {
    header: Header,
    /// The budget of the creating process, which its frames are charged
    /// to.
    budget: Arc<Budget>,
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
    /// in place of the shared zero page and whose translations of it are
    /// still being removed. Nothing may write to their frames or map them
    /// until then. Locked after the pages.
    changing: SpinLock<Vec<(u64, u64)>>,
    /// The seals of `mem_seal`, never removed. Checks and changes are
    /// serialized because system calls run one at a time on the one
    /// processor.
    seals: AtomicU32,
    /// For a paged object, what its pager needs.
    paged: Option<Paged>,
}

/// The part of a paged memory object that its pager drives.
pub struct Paged {
    /// The object ID of the pager.
    pager: u64,
    port: Weak<Port>,
    key: u64,
    /// Locked after the pages.
    state: SpinLock<Paging>,
    /// The budgets that frames supplied to the object stay charged to, by
    /// frame address.
    charges: SpinLock<Map<u64, Arc<Budget>>>,
    /// The byte range of the outstanding writeback, which its end must
    /// name exactly. Locked after the pages.
    writeback: SpinLock<Option<(u64, u64)>>,
}

/// The ops of `pager_op`.
pub const OP_FAIL: u32 = 1;
pub const OP_EVICT: u32 = 2;
pub const OP_WRITEBACK_BEGIN: u32 = 3;
pub const OP_WRITEBACK_END: u32 = 4;
pub const OP_WRITEBACK_ABORT: u32 = 5;

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
    unsafe { &mut *(space::direct(phys) as *mut [u8; PAGE_SIZE as usize]) }
}

/// The frame that pages of anonymous objects read before they are
/// written map. Nothing ever writes to it.
static ZERO_FRAME: SpinLock<Option<u64>> = SpinLock::new(None);

/// The shared zero page, allocated on first use.
fn zero_frame() -> Option<u64> {
    let mut zero = ZERO_FRAME.lock();
    if zero.is_none() {
        let phys = FRAMES.lock().as_mut().and_then(|f| f.alloc())?;
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
    /// charged to `budget`.
    pub fn new(budget: &Arc<Budget>, size: u64, options: u32) -> Result<Arc<MemoryObject>, Error> {
        let pages = Pages::new(size, options, MAX_SIZE)?;
        let charged = size_of::<MemoryObject>() as u64;
        budget.charge_kernel(charged)?;
        // The object releases the charge when it goes, also on failure.
        kernel_core::fallible::try_arc(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.clone(),
            charged,
            pages: SpinLock::new(pages),
            physical: None,
            mappings: SpinLock::new(nodes::map()),
            writable: AtomicU64::new(0),
            changing: SpinLock::new(Vec::new()),
            seals: AtomicU32::new(0),
            paged: None,
        })
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
        if options & !(RESIZABLE | TRACK_DIRTY) != 0 {
            return Err(Error::InvalidArgs);
        }
        let pages = Pages::new(size, options & RESIZABLE, MAX_SIZE)?;
        // With its entry in the pager's list of objects and that entry's
        // share of storage.
        let charged = (size_of::<MemoryObject>() + 2 * size_of::<Weak<MemoryObject>>()) as u64;
        budget.charge_kernel(charged)?;
        // The object releases the charge when it goes, also on failure.
        kernel_core::fallible::try_arc(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.clone(),
            charged,
            pages: SpinLock::new(pages),
            physical: None,
            mappings: SpinLock::new(nodes::map()),
            writable: AtomicU64::new(0),
            changing: SpinLock::new(Vec::new()),
            seals: AtomicU32::new(0),
            paged: Some(Paged {
                pager,
                port: Arc::downgrade(port),
                key,
                state: SpinLock::new(
                    Paging::new(options & TRACK_DIRTY != 0).charged_to(budget.clone()),
                ),
                charges: SpinLock::new(nodes::map()),
                writeback: SpinLock::new(None),
            }),
        })
    }

    pub fn is_paged(&self) -> bool {
        self.paged.is_some()
    }

    /// Whether the object is neither physical nor paged.
    pub fn is_anonymous(&self) -> bool {
        self.physical.is_none() && self.paged.is_none()
    }

    /// Fails with `AccessDenied` when the object is revoked.
    pub fn check_revoked(&self) -> Result<(), Error> {
        match &self.paged {
            Some(p) if p.state.lock().revoked() => Err(Error::AccessDenied),
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
        port.queue_kernel(packet, &self.budget)
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
            drop(state);
            drop(pages);
            match absent {
                Absent::Request => match self.request(paged, index) {
                    Ok(()) => {}
                    // The page is requested again by the next access,
                    // also by the threads waiting for it.
                    Err(Error::NoResources) => {
                        paged.state.lock().withdraw(index);
                        crate::pager::wake(koid, index, index + 1);
                        return Err(Error::NoResources);
                    }
                    Err(_) => {
                        // Without room to record the failure, the next
                        // access requests the page again.
                        let _ = paged.state.lock().fail(index, index + 1);
                        crate::pager::wake(koid, index, index + 1);
                        return Err(Error::Io);
                    }
                },
                Absent::Wait => {}
                Absent::Failed => return Err(Error::Io),
                Absent::Revoked => return Err(Error::AccessDenied),
            }
            // Nothing runs between the request and the registration, as
            // the kernel runs one thread at a time on the one processor.
            let thread = crate::pager::wait_for(koid, index)?;
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
            let _ = crate::sched::sleep_until(crate::time::now() + 1_000_000);
        }
    }

    /// Runs `op` of `pager_op` on the pages of the `len` bytes from
    /// `offset` of a paged object.
    pub fn pager_op(&self, op: u32, offset: u64, len: u64) -> Result<(), Error> {
        let paged = self.paged.as_ref().ok_or(Error::NotSupported)?;
        if !(OP_FAIL..=OP_WRITEBACK_ABORT).contains(&op) {
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
            OP_WRITEBACK_BEGIN => {
                {
                    let pages = self.pages.lock();
                    pages.check_range(offset, len)?;
                    paged.state.lock().writeback_open(first, end)?;
                    *paged.writeback.lock() = Some((offset, len));
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
                    let (recorded, done) = match state.writeback_record(ITEM_STEP) {
                        Ok(step) => step,
                        Err(e) => {
                            *paged.writeback.lock() = None;
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
                    if *paged.writeback.lock() != Some((offset, len)) {
                        return Err(Error::BadState);
                    }
                    state.writeback_close(first, end, op == OP_WRITEBACK_END)?;
                }
                // The recorded pages are handled in steps; the writeback
                // stays outstanding until the last.
                loop {
                    let mut state = paged.state.lock();
                    if state.writeback_finish(ITEM_STEP) {
                        *paged.writeback.lock() = None;
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
            if !state.evictable(index) || freed.try_reserve(1).is_err() {
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
    /// waits for kernel copies that hold its frames, removes every
    /// translation of it, and wakes the threads waiting for its pages.
    pub fn revoke(&self) -> Result<(), Error> {
        let paged = self.paged.as_ref().ok_or(Error::NotSupported)?;
        self.wait_unpinned();
        let count = {
            let pages = self.pages.lock();
            paged.state.lock().revoke();
            pages.count()
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
    /// budget.
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
        source.check_writable()?;
        self.check_revoked()?;
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
                for (index, phys) in (from..).zip(moved) {
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
                    if let Some((charge, page)) = supplied {
                        charges.insert(phys, source.budget.clone());
                        drop(charge);
                        pages.insert(index, phys, page);
                    } else {
                        leftover.push(phys);
                    }
                }
            }
            source.free(leftover);
            crate::pager::wake(self.header.koid(), from, to);
            refused
        })
    }

    /// Takes the frames of the `len` bytes from `offset`, all present,
    /// out of the object without freeing them or releasing their charges,
    /// and removes their translations. Fails with `BadState` while a
    /// reader or writer holds the object's frames.
    fn take_frames(&self, offset: u64, len: u64) -> Result<Vec<u64>, Error> {
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
    /// of the page size, mapped with `policy`, charged to `budget`.
    pub fn new_physical(
        budget: &Arc<Budget>,
        base: u64,
        size: u64,
        policy: Policy,
    ) -> Result<Arc<MemoryObject>, Error> {
        debug_assert!(base.is_multiple_of(PAGE_SIZE) && size.is_multiple_of(PAGE_SIZE));
        let pages = Pages::new(size, 0, MAX_SIZE)?;
        let charged = size_of::<MemoryObject>() as u64;
        budget.charge_kernel(charged)?;
        // The object releases the charge when it goes, also on failure.
        kernel_core::fallible::try_arc(MemoryObject {
            header: Header::new(ObjectType::MemoryObject),
            budget: budget.clone(),
            charged,
            pages: SpinLock::new(pages),
            physical: Some((base, policy)),
            mappings: SpinLock::new(nodes::map()),
            writable: AtomicU64::new(0),
            changing: SpinLock::new(Vec::new()),
            seals: AtomicU32::new(0),
            paged: None,
        })
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
        // that no output prepared before it is written after it.
        let _pages = loop {
            if write {
                self.wait_unpinned();
            }
            let pages = self.pages.lock();
            if !write || !pages.pinned() {
                break pages;
            }
        };
        if write && self.mapped_writable() {
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
    /// frames from `phys`, charging them to `budget`. The frames return
    /// to the frame allocator when the object is destroyed.
    pub fn from_frames(
        budget: &Arc<Budget>,
        phys: u64,
        count: u64,
    ) -> Result<Arc<MemoryObject>, Error> {
        budget.charge_pages(count)?;
        // The frames come back to the allocator with the object.
        if let Some(frames) = FRAMES.lock().as_mut() {
            frames.adopt(count);
        }
        if let Err(e) = budget.charge_kernel(count * PAGE_ENTRY_COST) {
            budget.release_pages(count);
            return Err(e);
        }
        let object = MemoryObject::new(budget, count * PAGE_SIZE, 0).inspect_err(|_| {
            budget.release_pages(count);
            budget.release_kernel(count * PAGE_ENTRY_COST);
        })?;
        let mut pages = object.pages.lock();
        for index in 0..count {
            match nodes::claim_insert() {
                Ok(claim) => pages.insert(index, phys + index * PAGE_SIZE, claim),
                Err(e) => {
                    // The object frees the frames it took, and the rest
                    // go back here.
                    drop(pages);
                    object.free((index..count).map(|i| phys + i * PAGE_SIZE));
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
    /// the zero page that stood in for it.
    fn alloc_changing(&self, pages: &mut Pages, index: u64) -> Result<u64, Error> {
        self.mark(index, index + 1)?;
        self.alloc(pages, index)
            .inspect_err(|_| self.unmark(index, index + 1))
    }

    /// Marks pages `first` to `end` as changing, or fails with `NoMemory`
    /// without room to. The pages are locked.
    fn mark(&self, first: u64, end: u64) -> Result<(), Error> {
        let mut changing = self.changing.lock();
        changing.try_reserve(1).map_err(|_| Error::NoMemory)?;
        changing.push((first, end));
        Ok(())
    }

    /// Ends the mark of `mark` on pages `first` to `end`, and wakes the
    /// threads waiting for them.
    fn unmark(&self, first: u64, end: u64) {
        let mut changing = self.changing.lock();
        let at = changing.iter().position(|&m| m == (first, end));
        changing.swap_remove(at.expect("the pages were marked"));
        drop(changing);
        crate::pager::wake(self.header.koid(), first, end);
    }

    /// Removes the old translations of pages `first` to `end`, marked by
    /// `mark` and then changed, and ends the mark. No lock may be held.
    fn settle(&self, first: u64, end: u64) {
        self.unmap_in_steps(first, end);
        self.unmark(first, end);
    }

    /// Whether page `index` is marked changing. The pages are locked.
    fn is_changing(&self, index: u64) -> bool {
        let changing = self.changing.lock();
        changing
            .iter()
            .any(|&(first, end)| (first..end).contains(&index))
    }

    /// Waits until page `index` is not marked changing. Fails with `Io`
    /// when the thread is killed meanwhile, and with `NoResources` when
    /// the wait cannot be charged.
    pub fn wait_settled(&self, index: u64) -> Result<(), Error> {
        let koid = self.header.koid();
        let _charge = crate::pager::charge_wait()?;
        loop {
            let pages = self.pages.lock();
            if !self.is_changing(index) {
                return Ok(());
            }
            let thread = crate::pager::wait_for(koid, index)?;
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
            // With dirty tracking, a page becomes writable through a
            // mapping only on a write, which marks it dirty.
            let writable =
                perms & PERM_WRITE != 0 && (write || state.writable_without_fault(index));
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
            (None, Some(phys)) => (phys, perms & PERM_WRITE != 0),
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
        let mut pages = self.pages.lock();
        let mine = |r: Option<&aspace::RegionOf>| {
            r.filter(|r| core::ptr::eq(&*r.mem, &**self))
                .map(|r| (r.page(virt), r.perms))
        };
        let Some((index, perms)) = space.with_region(virt, mine) else {
            return Err(Error::InvalidArgs);
        };
        if !aspace::allows(perms, true, false) || index >= pages.count() {
            return Err(Error::InvalidArgs);
        }
        if let Some((base, policy)) = self.physical {
            drop(pages);
            let virt = space::map_physical_page(base + index * PAGE_SIZE, policy)
                .ok_or(Error::NoMemory)?;
            return Ok(Held {
                mem: self.clone(),
                backing: Some(Backing::Mapped(virt)),
            });
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
                return self.hold_for_write(space, virt);
            };
            state.written(index)?;
            drop(state);
            pages.pin();
            return Ok(Held {
                mem: self.clone(),
                backing: Some(Backing::Frame(phys)),
            });
        }
        if self.is_changing(index) {
            drop(pages);
            self.wait_settled(index)?;
            return self.hold_for_write(space, virt);
        }
        let Some(phys) = pages.frame(index) else {
            self.alloc_changing(&mut pages, index)?;
            drop(pages);
            self.settle(index, index + 1);
            return self.hold_for_write(space, virt);
        };
        pages.pin();
        Ok(Held {
            mem: self.clone(),
            backing: Some(Backing::Frame(phys)),
        })
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
            let pages = self.pages.lock();
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
    /// budget, and returns it.
    fn alloc(&self, pages: &mut Pages, index: u64) -> Result<u64, Error> {
        let claim = nodes::claim_insert()?;
        self.budget.charge_pages(1)?;
        if let Err(e) = self.budget.charge_kernel(PAGE_ENTRY_COST) {
            self.budget.release_pages(1);
            return Err(e);
        }
        let Some(phys) = FRAMES.lock().as_mut().and_then(|f| f.alloc()) else {
            self.release(1);
            return Err(Error::NoMemory);
        };
        // SAFETY: the frame was just allocated, so nothing references it.
        unsafe { frame_bytes(phys) }.fill(0);
        pages.insert(index, phys, claim);
        Ok(phys)
    }

    /// Releases the charges of `count` frames.
    fn release(&self, count: u64) {
        self.budget.release_pages(count);
        self.budget.release_kernel(count * PAGE_ENTRY_COST);
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
    fn free(&self, frames: impl IntoIterator<Item = u64>) {
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
            let mut len = 0;
            for (slot, phys) in batch.iter_mut().zip(frames.by_ref()) {
                *slot = phys;
                len += 1;
            }
            if len == 0 {
                break;
            }
            since_check += len;
            let batch = &batch[..len];
            if let Some(allocator) = FRAMES.lock().as_mut() {
                for &phys in batch {
                    allocator.free(phys, 1);
                }
            }
            for &phys in batch {
                // A supplied frame's charge goes back to its own budget.
                match self
                    .paged
                    .as_ref()
                    .and_then(|p| p.charges.lock().remove(&phys))
                {
                    Some(budget) => {
                        budget.release_pages(1);
                        budget.release_kernel(PAGE_ENTRY_COST);
                    }
                    None => count += 1,
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
                        state.written(index)?;
                        phys
                    }
                    (None, Some(phys)) => phys,
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
            let freed = pages.decommit(from, end)?;
            drop(pages);
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
            let pages = self.pages.lock();
            if !pages.trimming() {
                break pages;
            }
            drop(pages);
            crate::sched::yield_now();
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
        let resized = pages.set_size(size, MAX_SIZE)?;
        if let Some((phys, from)) = resized.zero {
            // SAFETY: the pages are locked.
            let frame = unsafe { frame_bytes(phys) };
            frame[from..].fill(0);
        }
        let new = pages.count();
        if new >= old {
            return Ok(());
        }
        pages.set_trimming(true);
        drop(pages);
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
        let kind = if self.is_physical() {
            KIND_PHYSICAL
        } else if self.is_paged() {
            KIND_PAGED
        } else {
            KIND_ANONYMOUS
        };
        self.pages.lock().info(kind, self.seals())
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
}

impl Held {
    fn end<F: FnMut()>(&mut self, work: &mut Work<F>) {
        match self.backing.take() {
            Some(Backing::Frame(_)) => self.mem.unpin(work),
            Some(Backing::Mapped(virt)) => space::unmap_physical_page(virt),
            None => {}
        }
    }
}

impl crate::publish::Page for Held {
    fn base(&self) -> *mut u8 {
        match self.backing {
            Some(Backing::Frame(phys)) => space::direct(phys) as *mut u8,
            Some(Backing::Mapped(virt)) => virt as *mut u8,
            None => unreachable!("a released page is never written"),
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
                let from = (space::direct(phys) as *const u8).add(inside);
                core::ptr::copy_nonoverlapping(from, buf.as_mut_ptr(), buf.len());
            },
            None => buf.fill(0),
        }
    }

    /// Releases the frames, accounting for the work that takes.
    pub fn release<F: FnMut()>(mut self, work: &mut Work<F>) {
        if core::mem::take(&mut self.pinned) {
            self.mem.unpin(work);
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if core::mem::take(&mut self.pinned) {
            self.mem.unpin(&mut Work::new(|| {}));
        }
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
        let entry = kernel_core::fallible::try_box(Entry {
            mem: mem.clone(),
            key,
            writable,
        })?;
        let _claim = nodes::claim_insert()?;
        mem.mappings.lock().insert(key, space);
        if writable {
            mem.writable.fetch_add(1, Ordering::Relaxed);
        }
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
        self.budget.release_kernel(self.charged);
    }
}
