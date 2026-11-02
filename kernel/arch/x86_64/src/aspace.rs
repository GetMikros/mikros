//! User address spaces: their page tables, and loading them with PCIDs.

use crate::control::CR4_PCIDE;
use crate::cpu;
use crate::memory::FRAMES;
use crate::memory_object::{Mapping, MemoryObject};
use crate::paging::{self, Flags, Mapper, PAGE_SIZE, Policy, Tables};
use crate::percpu;
use crate::publish::ITEM_STEP;
use crate::space;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::object::budget::{Budget, Scratch};
use kernel_core::regions::{
    Charge, MapOptions, PERM_EXECUTE, PERM_READ, PERM_WRITE, Region, Regions, Removal,
};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The lowest user address.
pub const USER_START: u64 = 0x0000_0000_0001_0000;
/// The end of the user address range, exclusive.
pub const USER_END: u64 = 0x0000_7FFF_FFFF_F000;
/// The end of the range `BELOW_4G` mappings are placed in.
const END_4G: u64 = 0x1_0000_0000;

/// Whether the `len` bytes from `addr` lie in the user address range.
pub fn in_user_range(addr: u64, len: u64) -> bool {
    addr >= USER_START && addr.checked_add(len).is_some_and(|end| end <= USER_END)
}

const CR3_NO_FLUSH: u64 = 1 << 63;
/// The PCIDs 1 to `SLOTS` are handed to user address spaces; the kernel's
/// own tables use PCID 0.
const SLOTS: usize = 6;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Where a change of an address space invalidates the translations it
/// removes on this processor.
#[derive(Clone, Copy)]
enum Here {
    /// The space is loaded: by address, in the loaded PCID.
    Loaded,
    /// The space has this PCID, with its translations valid, but is not
    /// loaded: by address, with INVPCID.
    Pcid(u64),
    /// Nowhere: the space's PCID is flushed before its next load.
    Elsewhere,
}

impl Here {
    fn invalidate(self, virt: u64) {
        match self {
            Here::Loaded => cpu::invlpg(virt),
            Here::Pcid(pcid) => cpu::invpcid_address(pcid, virt),
            Here::Elsewhere => {}
        }
    }
}

#[derive(Clone, Copy)]
struct Slot {
    /// The address space that has the slot's PCID, or 0.
    id: u64,
    /// That space's generation when its translations were last flushed.
    generation: u64,
}

/// The PCIDs of one processor, and the address space it has loaded.
pub struct Pcids {
    slots: [Slot; SLOTS],
    /// The slot to hand out next.
    next: usize,
    /// The loaded address space, or 0 for the kernel's tables.
    current: u64,
}

impl Pcids {
    pub const fn new() -> Pcids {
        Pcids {
            slots: [Slot {
                id: 0,
                generation: 0,
            }; SLOTS],
            next: 0,
            current: 0,
        }
    }
}

fn pcids_enabled() -> bool {
    cpu::read_cr4() & CR4_PCIDE != 0
}

/// How a user page is mapped. Every mapping is readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub writable: bool,
    pub executable: bool,
    pub policy: Policy,
}

struct Inner {
    root: u64,
    /// Table frames charged to the budget.
    charged: u64,
}

/// The page tables of one process, and the regions that say what they
/// may map.
///
/// Locks are taken in this order: a memory object's pages, `regions`,
/// `inner`.
pub struct AddressSpace {
    id: u64,
    budget: Arc<Budget>,
    /// Bumped whenever a translation is removed, so that
    /// processors flush the space's PCID before using it again.
    generation: AtomicU64,
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

impl Tables for Charged<'_> {
    fn alloc(&mut self) -> Option<u64> {
        if let Some(budget) = self.budget
            && budget.charge_kernel(PAGE_SIZE).is_err()
        {
            self.refused = true;
            return None;
        }
        let frame = FRAMES.lock().as_mut().and_then(|f| f.alloc());
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

    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: the frame holds a table of this address space, which
        // the caller has locked, and the direct map covers it.
        unsafe { &mut *(space::direct(phys) as *mut [u64; 512]) }
    }
}

impl Charged<'_> {
    fn error(&self, e: paging::Error) -> Error {
        match e {
            paging::Error::OutOfMemory if self.refused => Error::NoResources,
            paging::Error::OutOfMemory => Error::NoMemory,
            paging::Error::AlreadyMapped(_) => Error::AlreadyExists,
            _ => Error::Internal,
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
        let regions = Regions::new(USER_START, END_4G, USER_END)?;
        let mut tables = Charged {
            budget: charge.then_some(&*budget),
            count: 0,
            refused: false,
        };
        let root = Mapper::user(&mut tables, space::root()).map(|m| m.root());
        let root = root.map_err(|e| tables.error(e))?;
        let charged = if charge { tables.count } else { 0 };
        Ok(AddressSpace {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            budget,
            generation: AtomicU64::new(0),
            regions: SpinLock::new(regions),
            inner: SpinLock::new(Inner { root, charged }),
        })
    }

    /// Maps the 4 KiB page at `virt` to the frame at `phys`. Tables it
    /// needs are charged to the budget when `charge` is set.
    pub fn map(&self, virt: u64, phys: u64, access: Access, charge: bool) -> Result<(), Error> {
        if !virt.is_multiple_of(PAGE_SIZE) || !in_user_range(virt, PAGE_SIZE) {
            return Err(Error::OutOfRange);
        }
        let flags = Flags {
            writable: access.writable,
            executable: access.executable,
            global: false,
            user: true,
            policy: access.policy,
        };
        let mut inner = self.inner.lock();
        let mut tables = Charged {
            budget: charge.then_some(&*self.budget),
            count: 0,
            refused: false,
        };
        let result =
            Mapper::existing(&mut tables, inner.root, false).map(virt, phys, PAGE_SIZE, flags);
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
        let mut tables = Charged {
            budget: None,
            count: 0,
            refused: false,
        };
        // Interrupts are off, so the space stays loaded or not loaded on
        // this processor while its pages are removed.
        let here = self.here();
        let mut pages = Mapper::existing(&mut tables, inner.root, false);
        let mut removed = false;
        pages
            .unmap_present(virt, size, |v, phys| {
                here.invalidate(v);
                removed = true;
                unmapped(phys);
            })
            .map_err(|_| Error::Internal)?;
        if removed {
            self.flush_elsewhere(here);
        }
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
        let mut tables = Charged {
            budget: None,
            count: 0,
            refused: false,
        };
        // Interrupts are off, so the space stays loaded or not loaded on
        // this processor while its pages are removed.
        let here = self.here();
        let mut pages = Mapper::existing(&mut tables, inner.root, false);
        let mut removed = false;
        let next = pages
            .unmap_present_some(virt, size, limit, |v, _| {
                here.invalidate(v);
                removed = true;
            })
            .map_err(|_| Error::Internal)?;
        if removed {
            self.flush_elsewhere(here);
        }
        Ok(next)
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
    /// charged to `budget`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_region(
        self: &Arc<Self>,
        options: MapOptions,
        addr: u64,
        size: u64,
        mem: &Arc<MemoryObject>,
        offset: u64,
        max: u32,
        budget: &Arc<Budget>,
    ) -> Result<u64, Error> {
        if options.specific && !in_user_range(addr, size) {
            return Err(Error::OutOfRange);
        }
        if size > USER_END - USER_START {
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
    /// the range is being removed.
    #[allow(clippy::too_many_arguments)]
    fn try_add_region(
        self: &Arc<Self>,
        options: MapOptions,
        addr: u64,
        size: u64,
        mem: &Arc<MemoryObject>,
        offset: u64,
        max: u32,
        budget: &Arc<Budget>,
    ) -> Result<u64, Error> {
        let charge = Charge::new(Some(budget))?;
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
        let start = start.max(USER_START);
        let end = end.min(USER_END);
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
        self.unmap_in_steps(USER_START, USER_END - USER_START)
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
        while let Some(next) =
            self.regions
                .lock()
                .check_range(at, end, ITEM_STEP, &ok, &mut refused)?
        {
            at = next;
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
        while let Some(next) = self
            .regions
            .lock()
            .change_some(at, end, ITEM_STEP, &mut change)
        {
            at = next;
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
    /// its turn comes, having changed meanwhile, keeps its permissions.
    /// Regions that splitting adds are charged to `budget`.
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
        self.change_in_steps(start, end, budget, |r| {
            if ok(r) {
                r.set_perms(perms);
            }
        })?;
        self.unmap_in_steps(start, end - start)
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

    /// How translations of the space that a change removes are
    /// invalidated on this processor.
    fn here(&self) -> Here {
        let pcids = percpu::this().pcids.lock();
        if pcids.current == self.id {
            return Here::Loaded;
        }
        // A PCID whose translations are all valid keeps them valid when
        // the removed ones are invalidated in it by address.
        let generation = self.generation.load(Ordering::Acquire);
        let current = |s: &Slot| s.id == self.id && s.generation == generation;
        match pcids.slots.iter().position(current) {
            Some(i) if pcids_enabled() && cpu::has_invpcid() => Here::Pcid(i as u64 + 1),
            _ => Here::Elsewhere,
        }
    }

    /// Makes processors flush the space's PCID before using it again,
    /// other than this one when `here` invalidated the removed
    /// translations in it.
    fn flush_elsewhere(&self, here: Here) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if matches!(here, Here::Elsewhere) {
            return;
        }
        let mut pcids = percpu::this().pcids.lock();
        let current = |s: &&mut Slot| s.id == self.id && s.generation == generation - 1;
        if let Some(slot) = pcids.slots.iter_mut().find(current) {
            slot.generation = generation;
        }
    }

    /// Loads the address space on this processor.
    pub fn activate(&self) {
        let root = self.inner.lock().root;
        let mut pcids = percpu::this().pcids.lock();
        if !pcids_enabled() {
            if pcids.current != self.id {
                // SAFETY: the tables share the kernel half.
                unsafe { cpu::write_cr3(root) };
                pcids.current = self.id;
            }
            return;
        }
        let generation = self.generation.load(Ordering::Acquire);
        let found = pcids.slots.iter().position(|s| s.id == self.id);
        let (index, flush) = match found {
            Some(i) => (i, pcids.slots[i].generation != generation),
            None => {
                let i = pcids.next;
                pcids.next = (i + 1) % SLOTS;
                (i, true)
            }
        };
        if !flush && pcids.current == self.id {
            return;
        }
        pcids.slots[index] = Slot {
            id: self.id,
            generation,
        };
        let pcid = index as u64 + 1;
        // The PCID's old translations go: with INVPCID before the load,
        // otherwise by loading without the no-flush bit.
        let no_flush = if !flush {
            CR3_NO_FLUSH
        } else if cpu::has_invpcid() {
            cpu::invpcid_context(pcid);
            CR3_NO_FLUSH
        } else {
            0
        };
        // SAFETY: the tables share the kernel half.
        unsafe { cpu::write_cr3(root | pcid | no_flush) };
        pcids.current = self.id;
    }
}

impl AddressSpace {
    /// Holds the page at `page` as the destination of a kernel write
    /// (`MemoryObject::hold_for_write`).
    pub fn hold_for_write(&self, page: u64) -> Result<crate::memory_object::Held, Error> {
        if !in_user_range(page, PAGE_SIZE) {
            return Err(Error::InvalidArgs);
        }
        let mem = self
            .with_region(page, |r| r.map(|r| r.mem.clone()))
            .ok_or(Error::InvalidArgs)?;
        mem.hold_for_write(self, page)
    }
}

const FAULT_WRITE: u64 = 1 << 1;
const FAULT_RESERVED: u64 = 1 << 3;
const FAULT_FETCH: u64 = 1 << 4;

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
}

/// Resolves a page fault at `addr`, raised by user code or by the user
/// copy routine, when a region allows the access.
pub fn resolve_fault(frame: &crate::idt::Frame, addr: u64) -> Fault {
    if frame.error & FAULT_RESERVED != 0
        || !in_user_range(addr, 1)
        || (frame.cs & 3 == 0 && !crate::usercopy::in_copy(frame))
    {
        return Fault::Denied;
    }
    let Some(process) = crate::sched::current().and_then(|t| t.process().cloned()) else {
        return Fault::Denied;
    };
    let aspace = process.aspace();
    let Some(mem) = aspace.with_region(addr, |r| r.map(|r| r.mem.clone())) else {
        return Fault::Denied;
    };
    let write = frame.error & FAULT_WRITE != 0;
    let fetch = frame.error & FAULT_FETCH != 0;
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

/// Loads the kernel's own tables on this processor.
pub fn activate_kernel() {
    let mut pcids = percpu::this().pcids.lock();
    if pcids.current == 0 {
        return;
    }
    let value = if pcids_enabled() {
        space::root() | CR3_NO_FLUSH
    } else {
        space::root()
    };
    // SAFETY: these are the kernel's own tables; PCID 0 only ever holds
    // their translations.
    unsafe { cpu::write_cr3(value) };
    pcids.current = 0;
}

impl Drop for AddressSpace {
    /// Frees the tables. Every user mapping must have been removed.
    fn drop(&mut self) {
        {
            let mut pcids = percpu::this().pcids.lock();
            if let Some(slot) = pcids.slots.iter_mut().find(|s| s.id == self.id) {
                slot.id = 0;
            }
            let loaded = pcids.current == self.id;
            drop(pcids);
            if loaded {
                activate_kernel();
            }
        }
        // Taken without the lock, so that the tables are freed with a
        // reschedule check every `ITEM_STEP` when no other lock is held.
        let inner = self.inner.get_mut();
        let mut tables = Charged {
            budget: None,
            count: 0,
            refused: false,
        };
        let mut count = 1;
        Mapper::existing(&mut tables, inner.root, false).free_lower_half(|table| {
            free_frame(table);
            count += 1;
            if count % ITEM_STEP as u64 == 0 {
                crate::sched::preempt_point_if_unlocked();
            }
        });
        free_frame(inner.root);
        debug_assert!(inner.charged <= count);
        self.budget.release_kernel(inner.charged * PAGE_SIZE);
    }
}
