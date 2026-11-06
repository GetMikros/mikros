//! The page tables of user address spaces on x86_64, and loading them
//! with PCIDs.

use crate::control::CR4_PCIDE;
use crate::cpu;
use crate::paging::{self, Flags, Mapper, PAGE_SIZE, Tables};
use crate::percpu;
use crate::space;
use crate::tlb::{self, CpuMask};
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::arch::{PageError, PageFlags};

/// The lowest user address.
pub const USER_START: u64 = 0x0000_0000_0001_0000;
/// The end of the user address range, exclusive.
pub const USER_END: u64 = 0x0000_7FFF_FFFF_F000;

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
    /// The loaded address space, or 0 for the kernel's tables, with its
    /// root table, its PCID or 0, and the set of processors that have it
    /// loaded, or null for the kernel's tables.
    current: u64,
    root: u64,
    pcid: u64,
    loaded: *const CpuMask,
}

// SAFETY: `loaded` points into the loaded space's tables, which are not
// freed while any processor has them loaded, and only this processor
// uses its PCIDs.
unsafe impl Send for Pcids {}

impl Pcids {
    pub const fn new() -> Pcids {
        Pcids {
            slots: [Slot {
                id: 0,
                generation: 0,
            }; SLOTS],
            next: 0,
            current: 0,
            root: 0,
            pcid: 0,
            loaded: core::ptr::null(),
        }
    }

    /// Records that this processor, `me`, now has `id` loaded from `root`
    /// with `pcid`, and leaves the set of the space it had loaded.
    fn set_current(&mut self, me: usize, id: u64, root: u64, pcid: u64, loaded: *const CpuMask) {
        if self.current != id && !self.loaded.is_null() {
            // SAFETY: the space that was loaded is not freed before this
            // processor leaves its set.
            unsafe { (*self.loaded).remove(me) };
        }
        self.current = id;
        self.root = root;
        self.pcid = pcid;
        self.loaded = loaded;
    }
}

/// The most removed pages whose frames wait for a shootdown.
const BATCH: usize = 32;

fn pcids_enabled() -> bool {
    cpu::read_cr4() & CR4_PCIDE != 0
}

/// Table frames from a caller's allocator, reached through the direct
/// map.
struct Frames<'a>(&'a mut dyn FnMut() -> Option<u64>);

impl Tables for Frames<'_> {
    fn alloc(&mut self) -> Option<u64> {
        (self.0)()
    }

    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: the frame holds a table of an address space, whose
        // owner keeps other changes out, and the direct map covers it.
        unsafe { &mut *(space::direct(phys) as *mut [u64; 512]) }
    }
}

fn error(e: paging::Error) -> PageError {
    match e {
        paging::Error::OutOfMemory => PageError::OutOfMemory,
        paging::Error::AlreadyMapped(_) => PageError::AlreadyMapped,
        _ => PageError::Refused,
    }
}

/// The page tables of one user address space.
pub struct SpaceTables {
    /// The space's identity in the PCID slots, never 0.
    id: u64,
    /// The processors that have the space loaded, or are loading it.
    loaded: CpuMask,
    /// Bumped whenever a translation is removed, so that processors flush
    /// the space's PCID before using it again.
    generation: AtomicU64,
    root: u64,
}

impl SpaceTables {
    /// New tables, with the root table from `alloc`. The state is
    /// allocated before the root, so that a failure leaves no frame or
    /// charge behind.
    pub fn new(alloc: &mut dyn FnMut() -> Option<u64>) -> Result<Box<SpaceTables>, PageError> {
        let mut tables = kernel_core::fallible::try_box(SpaceTables {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            loaded: CpuMask::new(),
            generation: AtomicU64::new(0),
            root: 0,
        })
        .map_err(|_| PageError::OutOfMemory)?;
        tables.root = Mapper::user(&mut Frames(alloc), space::root())
            .map(|m| m.root())
            .map_err(error)?;
        Ok(tables)
    }

    pub fn map(
        &self,
        virt: u64,
        phys: u64,
        flags: PageFlags,
        alloc: &mut dyn FnMut() -> Option<u64>,
    ) -> Result<(), PageError> {
        let flags = Flags {
            writable: flags.writable,
            executable: flags.executable,
            global: false,
            user: true,
            policy: flags.policy,
        };
        Mapper::existing(&mut Frames(alloc), self.root, false)
            .map(virt, phys, PAGE_SIZE, flags)
            .map_err(error)
    }

    pub fn unmap(
        &self,
        virt: u64,
        size: u64,
        limit: usize,
        unmapped: &mut dyn FnMut(u64),
    ) -> Result<Option<u64>, PageError> {
        let mut none = || None;
        // Interrupts are off, so the space stays loaded or not loaded on
        // this processor while its pages are removed.
        let here = self.here();
        // The frames are passed on only once no processor can use the
        // translations, a batch at a time.
        let mut batch = [0u64; BATCH];
        let mut count = 0;
        let next = Mapper::existing(&mut Frames(&mut none), self.root, false)
            .unmap_present_some(virt, size, limit, |v, phys| {
                here.invalidate(v);
                batch[count] = phys;
                count += 1;
                if count == BATCH {
                    self.flush_removed(here, &batch, unmapped);
                    count = 0;
                }
            })
            .map_err(error);
        if count != 0 {
            self.flush_removed(here, &batch[..count], unmapped);
        }
        next
    }

    /// Drops the removed translations on every processor, then passes
    /// `frames` to `unmapped`.
    fn flush_removed(&self, here: Here, frames: &[u64], unmapped: &mut dyn FnMut(u64)) {
        self.flush_elsewhere(here);
        tlb::shootdown(self.id, 0, 0, false, &self.loaded);
        for &frame in frames {
            unmapped(frame);
        }
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
        let me = percpu::index() as usize;
        let mut pcids = percpu::this().pcids.lock();
        // The processor joins the set before it reads the generation, and
        // a change bumps the generation before it reads the set, so that
        // either this load flushes or the change sends a shootdown here.
        self.loaded.insert(me);
        if !pcids_enabled() {
            if pcids.current != self.id {
                // SAFETY: the tables share the kernel half.
                unsafe { cpu::write_cr3(self.root) };
                pcids.set_current(me, self.id, self.root, 0, &self.loaded);
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
        unsafe { cpu::write_cr3(self.root | pcid | no_flush) };
        pcids.set_current(me, self.id, self.root, pcid, &self.loaded);
    }

    /// Frees the tables, passing each table frame to `free`. No user page
    /// may be mapped any more.
    pub fn free(self, free: &mut dyn FnMut(u64)) {
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
        // Processors that still have the space loaded, idle or running
        // kernel threads since, switch to the kernel's tables.
        tlb::shootdown(self.id, 0, 0, true, &self.loaded);
        let mut none = || None;
        Mapper::existing(&mut Frames(&mut none), self.root, false).free_lower_half(&mut *free);
        free(self.root);
    }
}

/// Loads the kernel's own tables on this processor.
pub fn activate_kernel() {
    let mut pcids = percpu::this().pcids.lock();
    if pcids.current == 0 {
        return;
    }
    load_kernel(&mut pcids);
}

/// Loads the kernel's own tables with this processor's `pcids` locked.
fn load_kernel(pcids: &mut Pcids) {
    let value = if pcids_enabled() {
        space::root() | CR3_NO_FLUSH
    } else {
        space::root()
    };
    // SAFETY: these are the kernel's own tables; PCID 0 only ever holds
    // their translations.
    unsafe { cpu::write_cr3(value) };
    pcids.set_current(
        percpu::index() as usize,
        0,
        space::root(),
        0,
        core::ptr::null(),
    );
}

/// Handles a shootdown of user space `id` on this processor: when it has
/// the space loaded, drops all its translations, or switches to the
/// kernel's tables when the space is being freed.
pub fn serve_shootdown(id: u64, free: bool) {
    let mut pcids = percpu::this().pcids.lock();
    if pcids.current != id {
        return;
    }
    if free {
        load_kernel(&mut pcids);
    } else if pcids.pcid == 0 {
        // SAFETY: reloading the loaded tables drops their translations.
        unsafe { cpu::write_cr3(pcids.root) };
    } else if cpu::has_invpcid() {
        cpu::invpcid_context(pcids.pcid);
    } else {
        // SAFETY: as above; without the no-flush bit the load drops the
        // PCID's translations.
        unsafe { cpu::write_cr3(pcids.root | pcids.pcid) };
    }
}
