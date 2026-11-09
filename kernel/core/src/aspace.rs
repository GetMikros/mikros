//! User address spaces: the regions that say what they may map, and
//! their page tables.

use crate::arch::{PageError, PageTable};
use crate::frames::FRAMES;
use crate::memory_object::{Mapping, MemoryObject};
use crate::object::budget::{Budget, Scratch};
use crate::publish::ITEM_STEP;
use crate::regions::{
    Charge, MapOptions, PAGE_SIZE, PERM_EXECUTE, PERM_READ, PERM_WRITE, Region, Regions, Removal,
};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicU64, Ordering};

/// The user address range, from its lowest address to the one past it.
pub fn user_range() -> (u64, u64) {
    crate::arch::user_range()
}
/// The end of the range `BELOW_4G` mappings are placed in.
const END_4G: u64 = 0x1_0000_0000;

/// Whether the `len` bytes from `addr` lie in the user address range.
pub fn in_user_range(addr: u64, len: u64) -> bool {
    let (start, end) = user_range();
    addr >= start && addr.checked_add(len).is_some_and(|e| e <= end)
}

/// How a user page is mapped. Every mapping is readable.
pub type Access = crate::arch::PageFlags;

struct Inner {
    /// Freed when the space is dropped.
    tables: ManuallyDrop<PageTable>,
    /// Table frames charged to the budget.
    charged: u64,
}

/// The page tables of one process, and the regions that say what they
/// may map.
///
/// Locks are taken in this order: a memory object's pages, `regions`,
/// `inner`.
pub struct AddressSpace {
    budget: Arc<Budget>,
    regions: SpinLock<Regions<Arc<MemoryObject>, Mapping>>,
    inner: SpinLock<Inner>,
}

/// The region type of a user address space.
pub type RegionOf = Region<Arc<MemoryObject>, Mapping>;

/// A part of a range of an address space that one region maps.
pub struct Piece {
    pub mem: Arc<MemoryObject>,
    /// The offset in the object of the part's first byte, and its length.
    pub offset: u64,
    pub len: u64,
    /// The region's permissions and the most its handle allowed.
    pub perms: u32,
    pub max: u32,
}

/// A random value from the kernel's random number generator, which is
/// seeded during boot.
fn random() -> Result<u64, Error> {
    let mut bytes = [0u8; 8];
    crate::random::fill(&mut bytes).map_err(|_| Error::Internal)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Page table frames from the frame allocator, optionally charged to a
/// budget.
struct Charged<'b> {
    budget: Option<&'b Budget>,
    count: u64,
    /// Set when the budget refused a charge.
    refused: bool,
}

impl Charged<'_> {
    fn alloc(&mut self) -> Option<u64> {
        if let Some(budget) = self.budget
            && budget.charge_kernel(PAGE_SIZE).is_err()
        {
            self.refused = true;
            return None;
        }
        let frame = crate::frames::alloc_frame();
        match frame {
            Some(_) => self.count += 1,
            None => {
                if let Some(budget) = self.budget {
                    budget.release_kernel(PAGE_SIZE);
                }
            }
        }
        frame
    }

    fn error(&self, e: PageError) -> Error {
        match e {
            PageError::OutOfMemory if self.refused => Error::NoResources,
            PageError::OutOfMemory => Error::NoMemory,
            PageError::AlreadyMapped => Error::AlreadyExists,
            PageError::Refused => Error::Internal,
        }
    }
}

/// A new removal ID, never 0.
fn next_removal() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn free_frame(phys: u64) {
    if let Some(frames) = FRAMES.lock().as_mut() {
        frames.free(phys, 1);
    }
}

impl AddressSpace {
    /// An address space with no user mappings, belonging to `budget`. Its
    /// tables are charged to the budget when `charge` is set.
    pub fn new(budget: Arc<Budget>, charge: bool) -> Result<AddressSpace, Error> {
        let (start, end) = user_range();
        let regions = Regions::new(start, END_4G, end)?;
        let mut tables = Charged {
            budget: charge.then_some(&*budget),
            count: 0,
            refused: false,
        };
        // Tables that cannot be made give back the frames they took, whose
        // charges go here.
        let page_tables = PageTable::new(&mut || tables.alloc()).map_err(|e| {
            if let Some(budget) = tables.budget {
                budget.release_kernel(tables.count * PAGE_SIZE);
            }
            tables.error(e)
        });
        let page_tables = page_tables?;
        let charged = if charge { tables.count } else { 0 };
        Ok(AddressSpace {
            budget,
            regions: SpinLock::new(crate::lock_order::ASPACE_REGIONS, regions),
            inner: SpinLock::new(
                crate::lock_order::ASPACE_INNER,
                Inner {
                    tables: ManuallyDrop::new(page_tables),
                    charged,
                },
            ),
        })
    }

    /// Maps the 4 KiB page at `virt` to the frame at `phys`. Tables it
    /// needs are charged to the budget when `charge` is set.
    pub fn map(&self, virt: u64, phys: u64, access: Access, charge: bool) -> Result<(), Error> {
        if !virt.is_multiple_of(PAGE_SIZE) || !in_user_range(virt, PAGE_SIZE) {
            return Err(Error::OutOfRange);
        }
        let mut inner = self.inner.lock();
        let mut tables = Charged {
            budget: charge.then_some(&*self.budget),
            count: 0,
            refused: false,
        };
        let result = inner.tables.map(virt, phys, access, &mut || tables.alloc());
        if charge {
            inner.charged += tables.count;
        }
        result.map_err(|e| tables.error(e))
    }

    /// Removes the mapped pages among the `size` bytes from `virt`, and
    /// calls `unmapped` with the frame of each. When it returns, no
    /// processor uses the removed translations.
    pub fn unmap(&self, virt: u64, size: u64, mut unmapped: impl FnMut(u64)) -> Result<(), Error> {
        if !virt.is_multiple_of(PAGE_SIZE)
            || !size.is_multiple_of(PAGE_SIZE)
            || !in_user_range(virt, size)
        {
            return Err(Error::OutOfRange);
        }
        let inner = self.inner.lock();
        inner
            .tables
            .unmap(virt, size, usize::MAX, &mut unmapped)
            .map_err(|_| Error::Internal)?;
        Ok(())
    }

    /// Removes the translations from `virt` to `virt + size`, both
    /// multiples of the page size inside the user range, in steps of at
    /// most `ITEM_STEP` entries with a reschedule check between two. No
    /// lock may be held.
    pub fn unmap_in_steps(&self, virt: u64, size: u64) -> Result<(), Error> {
        let end = virt + size;
        let mut at = virt;
        loop {
            match self.unmap_some(at, end - at, ITEM_STEP)? {
                Some(next) => at = next,
                None => return Ok(()),
            }
            crate::sched::preempt_point();
        }
    }

    /// Does what `unmap` does with no callback, but visits at most
    /// `limit` entries and returns the address it stopped at when the
    /// range goes on.
    fn unmap_some(&self, virt: u64, size: u64, limit: usize) -> Result<Option<u64>, Error> {
        if !virt.is_multiple_of(PAGE_SIZE)
            || !size.is_multiple_of(PAGE_SIZE)
            || !in_user_range(virt, size)
        {
            return Err(Error::OutOfRange);
        }
        let inner = self.inner.lock();
        inner
            .tables
            .unmap(virt, size, limit, &mut |_| {})
            .map_err(|_| Error::Internal)
    }

    /// Chooses a start for a mapping of `size` bytes, a multiple of the
    /// page size, as `add_region` does without `SPECIFIC`.
    pub fn choose(&self, size: u64) -> Result<u64, Error> {
        self.regions.lock().choose(size, false, random)
    }

    /// Maps the `size` bytes of `mem` from `offset`, a multiple of the
    /// page size, with `options`: at `addr` when they are `SPECIFIC`,
    /// otherwise at a random free address. Returns the start. Pages are
    /// entered into the tables only when they are accessed. The region is
    /// charged to `budget` if given. Fails with `AccessDenied` when `mem`
    /// is revoked, or has the `WRITE` seal and the mapping is writable.
    #[allow(clippy::too_many_arguments)]
    pub fn add_region(
        self: &Arc<Self>,
        options: MapOptions,
        addr: u64,
        size: u64,
        mem: &Arc<MemoryObject>,
        offset: u64,
        max: u32,
        budget: Option<&Arc<Budget>>,
    ) -> Result<u64, Error> {
        if options.specific && !in_user_range(addr, size) {
            return Err(Error::OutOfRange);
        }
        let (start, end) = user_range();
        if size > end - start {
            return Err(Error::OutOfRange);
        }
        loop {
            let result = self.try_add_region(options, addr, size, mem, offset, max, budget);
            // The range is being removed; the insert waits until it is.
            if result != Err(Error::ShouldWait) {
                return result;
            }
            crate::sched::yield_now();
        }
    }

    /// Does what `add_region` does once, failing with `ShouldWait` when
    /// the range is being removed, and with `AccessDenied` when `mem` is
    /// revoked or the mapping is writable and `mem` has the `WRITE` seal.
    #[allow(clippy::too_many_arguments)]
    fn try_add_region(
        self: &Arc<Self>,
        options: MapOptions,
        addr: u64,
        size: u64,
        mem: &Arc<MemoryObject>,
        offset: u64,
        max: u32,
        budget: Option<&Arc<Budget>>,
    ) -> Result<u64, Error> {
        let charge = Charge::new(budget)?;
        // `vm_map` checked both before it waited to prepare its output, and
        // a seal or a revocation may have come meanwhile. Nothing between
        // here and the insert waits, so none can come in between.
        mem.check_revoked()?;
        if options.perms & PERM_WRITE != 0 {
            mem.check_writable()?;
        }
        let mut regions = self.regions.lock();
        let start = if options.specific {
            Ok(addr)
        } else {
            regions.choose(size, options.below_4g, random)
        };
        let result = start.and_then(|start| {
            let record = mem.record(self, start, options.perms & PERM_WRITE != 0)?;
            regions.insert(Region {
                start,
                len: size,
                offset,
                perms: options.perms,
                max,
                inheritance: options.inheritance,
                mem: mem.clone(),
                removal: 0,
                charge,
                record,
            })?;
            Ok(start)
        });
        // A region that was not added goes, with its record, after the
        // lock, as it may hold the last reference to the object.
        drop(regions);
        result
    }

    /// Removes the parts of regions from `start` to `end` and their pages.
    /// Regions the splits at the ends add are charged to `budget`. Fails
    /// with `NoMemory`, removing nothing, without room to split the
    /// regions at the ends or to record the removal, and with
    /// `NoResources` when the budget refuses the charge. No lock may be
    /// held.
    pub fn remove_regions(&self, start: u64, end: u64, budget: &Arc<Budget>) -> Result<(), Error> {
        let (lowest, past) = user_range();
        let start = start.max(lowest);
        let end = end.min(past);
        if start >= end {
            return Ok(());
        }
        let removal =
            self.regions
                .lock()
                .begin_removal(start, end, next_removal(), Some(budget))?;
        self.remove_in_steps(removal);
        Ok(())
    }

    /// Removes every region and its pages, as `remove_regions` does with
    /// the whole user range, which cannot fail. This also removes the
    /// translations made outside regions, whose frames belong to whoever
    /// mapped them and are not freed.
    pub fn remove_all_regions(&self) {
        let removal = self.regions.lock().begin_removal_of_all(next_removal());
        self.remove_in_steps(removal);
        let (start, end) = user_range();
        self.unmap_in_steps(start, end - start)
            .expect("the user range is page-aligned");
    }

    /// Does the steps of `removal`: marks up to `ITEM_STEP` regions at a
    /// time, which then stay, hidden from faults and changes, while their
    /// translations go, so that an object that loses pages meanwhile still
    /// finds them, and then takes them out one at a time. Reschedule
    /// checks come between the steps, with no lock held.
    fn remove_in_steps(&self, mut removal: Removal) {
        loop {
            let step = self.regions.lock().mark_some(&mut removal, ITEM_STEP);
            let Some((from, to)) = step else {
                break;
            };
            let mut at = from;
            loop {
                let next = self.regions.lock().next_marked(&removal, at, to);
                let Some((start, len)) = next else {
                    break;
                };
                self.unmap_in_steps(start, len)
                    .expect("regions are page-aligned and in the user range");
                at = start + len;
            }
            loop {
                let part = self.regions.lock().take_marked(&removal, from, to);
                // Dropping a region can destroy its memory object, which
                // takes the object's locks, so it happens with the regions
                // unlocked.
                if part.is_none() {
                    break;
                }
            }
            crate::sched::preempt_point();
        }
        self.regions.lock().end_removal(removal);
    }

    /// Checks the range from `start` to `end` in steps of `ITEM_STEP`
    /// regions with reschedule checks between them, as `check_range`
    /// does with `ok`. Fails with `NotFound` when a page is in no region,
    /// and otherwise with `AccessDenied` when `ok` refuses a region.
    fn check_in_steps(
        &self,
        start: u64,
        end: u64,
        ok: impl Fn(&RegionOf) -> bool,
    ) -> Result<(), Error> {
        let mut at = start;
        let mut refused = false;
        loop {
            // The regions are unlocked at the end of the statement, before
            // the reschedule check.
            let step = self
                .regions
                .lock()
                .check_range(at, end, ITEM_STEP, &ok, &mut refused)?;
            let Some(next) = step else {
                break;
            };
            at = next;
            #[cfg(feature = "fuzz")]
            crate::fuzz::pass(crate::fuzz::REGION_CHECK);
            crate::sched::preempt_point();
        }
        if refused {
            return Err(Error::AccessDenied);
        }
        Ok(())
    }

    /// Splits the regions at the ends of the range from `start` to `end`,
    /// charging `budget`, and then calls `change` with every region in it
    /// that is not being removed, in steps of `ITEM_STEP` regions with
    /// reschedule checks between them. Regions that changed since the
    /// range was checked are handed to `change` as they are now.
    fn change_in_steps(
        &self,
        start: u64,
        end: u64,
        budget: &Arc<Budget>,
        mut change: impl FnMut(&mut RegionOf),
    ) -> Result<(), Error> {
        self.regions.lock().split_range(start, end, Some(budget))?;
        let mut at = start;
        loop {
            // As in `check_in_steps`, the regions are unlocked before the
            // reschedule check.
            let step = self
                .regions
                .lock()
                .change_some(at, end, ITEM_STEP, &mut change);
            let Some(next) = step else {
                break;
            };
            at = next;
            #[cfg(feature = "fuzz")]
            crate::fuzz::pass(crate::fuzz::REGION_CHANGE);
            crate::sched::preempt_point();
        }
        Ok(())
    }

    /// Sets the inheritance of every page from `start` to `end`, or fails
    /// with `NotFound` when one is not mapped. Regions that splitting adds
    /// are charged to `budget`.
    pub fn set_inheritance(
        &self,
        start: u64,
        end: u64,
        inheritance: u32,
        budget: &Arc<Budget>,
    ) -> Result<(), Error> {
        self.check_in_steps(start, end, |_| true)?;
        self.change_in_steps(start, end, budget, |r| r.inheritance = inheritance)
    }

    /// Sets `perms` on every page from `start` to `end`, as `vm_protect`
    /// does, and removes their translations, so that the next access
    /// enters them with the new permissions. `allowed` decides whether a
    /// region's object accepts them; a region that no longer does when
    /// its turn comes, having changed meanwhile, keeps its permissions,
    /// and the call then fails with `AccessDenied` once the other regions
    /// have changed. Regions that splitting adds are charged to `budget`.
    pub fn protect(
        &self,
        start: u64,
        end: u64,
        perms: u32,
        allowed: impl Fn(&RegionOf) -> bool,
        budget: &Arc<Budget>,
    ) -> Result<(), Error> {
        let ok = |r: &RegionOf| perms & !r.max == 0 && allowed(r);
        self.check_in_steps(start, end, ok)?;
        let mut refused = false;
        self.change_in_steps(start, end, budget, |r| {
            if ok(r) {
                r.set_perms(perms);
            } else {
                refused = true;
            }
        })?;
        self.unmap_in_steps(start, end - start)?;
        match refused {
            true => Err(Error::AccessDenied),
            false => Ok(()),
        }
    }

    /// Removes the translations of pages `first` to `end`, exclusive, of
    /// `mem` through the region from `start`, if it still maps `mem`, in
    /// steps with reschedule checks. No lock may be held.
    pub fn unmap_mapping_in_steps(&self, mem: &MemoryObject, start: u64, first: u64, end: u64) {
        let range = {
            let regions = self.regions.lock();
            let region = regions.starting_at(start);
            region
                .filter(|r| core::ptr::eq(&*r.mem, mem))
                .and_then(|r| {
                    let base = r.offset / PAGE_SIZE;
                    let pages = r.len / PAGE_SIZE;
                    let from = first.saturating_sub(base).min(pages);
                    let to = end.saturating_sub(base).min(pages);
                    (from < to).then(|| (r.start + from * PAGE_SIZE, (to - from) * PAGE_SIZE))
                })
        };
        // A region that goes meanwhile has its translations removed by
        // its removal too; removing them twice does no harm.
        if let Some((at, len)) = range {
            self.unmap_in_steps(at, len)
                .expect("regions are page-aligned and in the user range");
        }
    }

    /// Calls `f` with the region containing `addr`, if any, while the
    /// regions are locked.
    pub fn with_region<R>(&self, addr: u64, f: impl FnOnce(Option<&RegionOf>) -> R) -> R {
        f(self.regions.lock().find(addr))
    }

    /// The parts of the `len` bytes from `addr` that regions not being
    /// removed map, in address order, listed in room charged to `scratch`
    /// in steps of `ITEM_STEP` regions with reschedule checks between
    /// them, and whether any byte of the range lies outside them.
    pub fn pieces(
        &self,
        addr: u64,
        len: u64,
        scratch: &mut Scratch,
    ) -> Result<(Vec<Piece>, bool), Error> {
        let Some(end) = addr.checked_add(len) else {
            return Ok((Vec::new(), true));
        };
        let mut pieces = Vec::new();
        let mut gap = false;
        let mut at = addr;
        while at < end {
            {
                let regions = self.regions.lock();
                for r in regions.overlapping(at, end).take(ITEM_STEP) {
                    let (from, to) = (r.start.max(at), r.end().min(end));
                    gap |= r.start > at || r.removal != 0;
                    if r.removal == 0 {
                        scratch.reserve(&mut pieces, 1)?;
                        pieces.push(Piece {
                            mem: r.mem.clone(),
                            offset: r.offset + (from - r.start),
                            len: to - from,
                            perms: r.perms,
                            max: r.max,
                        });
                    }
                    at = to;
                }
                if regions.overlapping(at, end).next().is_none() {
                    gap |= at < end;
                    break;
                }
            }
            crate::sched::preempt_point();
        }
        Ok((pieces, gap))
    }

    /// Maps the page at `virt` to `phys`, replacing what it mapped.
    pub fn replace(&self, virt: u64, phys: u64, access: Access) -> Result<(), Error> {
        self.unmap(virt, PAGE_SIZE, |_| {})?;
        self.map(virt, phys, access, true)
    }

    /// Loads the address space on this processor.
    pub fn activate(&self) {
        self.inner.lock().tables.activate();
    }
}

impl AddressSpace {
    /// Holds the page at `page` as the destination of a kernel write
    /// (`MemoryObject::hold_for_write`).
    pub fn hold_for_write(&self, page: u64) -> Result<crate::memory_object::Held, Error> {
        self.region_object(page)?.hold_for_write(self, page)
    }

    /// Holds the page at `page` as the source of a kernel read
    /// (`MemoryObject::hold_for_read_at`).
    pub fn hold_for_read(&self, page: u64) -> Result<crate::memory_object::Held, Error> {
        self.region_object(page)?.hold_for_read_at(self, page)
    }

    /// The object of the region at user page `page`, or `InvalidArgs`.
    fn region_object(&self, page: u64) -> Result<Arc<MemoryObject>, Error> {
        if !in_user_range(page, PAGE_SIZE) {
            return Err(Error::InvalidArgs);
        }
        self.with_region(page, |r| r.map(|r| r.mem.clone()))
            .ok_or(Error::InvalidArgs)
    }
}

/// How a page fault on a user page ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Resolved,
    /// No region allows the access.
    Denied,
    /// A region allows it, but no frame or page table could be had.
    OutOfMemory,
    /// The page of a paged object is absent and must be obtained first.
    Absent(u64),
    /// The page was given a frame in place of the zero page, whose
    /// translations must go before the access is tried again.
    Changed(u64),
    /// The page is changing; the access waits until it has changed.
    Busy(u64),
    /// A write to the page of a `STABLE` paged object waits until the
    /// writeback that records it ends.
    Stable(u64),
}

/// Resolves a page fault of the running thread at the user address
/// `addr`, raised by user code or by a copy from or to user memory, when
/// a region allows the access: a write if `write`, an instruction fetch if
/// `fetch`.
pub fn resolve_fault(addr: u64, write: bool, fetch: bool) -> Fault {
    if !in_user_range(addr, 1) {
        return Fault::Denied;
    }
    let Some(process) = crate::sched::current().and_then(|t| t.process().cloned()) else {
        return Fault::Denied;
    };
    let aspace = process.aspace();
    let Some(mem) = aspace.with_region(addr, |r| r.map(|r| r.mem.clone())) else {
        return Fault::Denied;
    };
    // An absent page of a paged object is waited for, outside the
    // object's locks, and the access tried again.
    loop {
        match mem.fault(aspace, addr & !(PAGE_SIZE - 1), write, fetch) {
            Fault::Absent(index) => match mem.obtain(index) {
                Ok(()) => {}
                Err(Error::NoResources) => return Fault::OutOfMemory,
                Err(_) => return Fault::Denied,
            },
            Fault::Changed(index) => mem.settle_page(index),
            Fault::Busy(index) => match mem.wait_settled(index) {
                Ok(()) => {}
                Err(Error::NoResources) => return Fault::OutOfMemory,
                Err(_) => return Fault::Denied,
            },
            Fault::Stable(index) => match mem.wait_stable(index) {
                Ok(()) => {}
                Err(Error::NoResources) => return Fault::OutOfMemory,
                Err(_) => return Fault::Denied,
            },
            result => return result,
        }
    }
}

/// Whether a region with `perms` allows a write or fetch as given.
pub fn allows(perms: u32, write: bool, fetch: bool) -> bool {
    perms & PERM_READ != 0
        && (!write || perms & PERM_WRITE != 0)
        && (!fetch || perms & PERM_EXECUTE != 0)
}

impl Drop for AddressSpace {
    /// Frees the tables. Every user mapping must have been removed.
    fn drop(&mut self) {
        // Taken without the lock, so that the tables are freed with a
        // reschedule check every `ITEM_STEP` when no other lock is held.
        let inner = self.inner.get_mut();
        // SAFETY: the space is being dropped, so nothing uses its tables
        // after this.
        let tables = unsafe { ManuallyDrop::take(&mut inner.tables) };
        // The root, which `free` passes last, counts with the tables.
        let mut count = 0;
        tables.free(&mut |table| {
            free_frame(table);
            count += 1;
            if count % ITEM_STEP as u64 == 0 {
                crate::sched::preempt_point_if_unlocked();
            }
        });
        debug_assert!(inner.charged <= count);
        self.budget.release_kernel(inner.charged * PAGE_SIZE);
    }
}
