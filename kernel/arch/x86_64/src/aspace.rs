//! User address spaces: their page tables, and loading them with PCIDs.

use crate::control::CR4_PCIDE;
use crate::cpu;
use crate::memory::FRAMES;
use crate::paging::{self, Flags, Mapper, PAGE_SIZE, Policy, Tables};
use crate::percpu;
use crate::space;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::object::budget::Budget;
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The lowest user address.
pub const USER_START: u64 = 0x0000_0000_0001_0000;
/// The end of the user address range, exclusive.
pub const USER_END: u64 = 0x0000_7FFF_FFFF_F000;

/// Whether the `len` bytes from `addr` lie in the user address range.
pub fn in_user_range(addr: u64, len: u64) -> bool {
    addr >= USER_START && addr.checked_add(len).is_some_and(|end| end <= USER_END)
}

const CR3_NO_FLUSH: u64 = 1 << 63;
/// The PCIDs 1 to `SLOTS` are handed to user address spaces; the kernel's
/// own tables use PCID 0.
const SLOTS: usize = 6;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

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

/// The page tables of one process.
pub struct AddressSpace {
    id: u64,
    budget: Arc<Budget>,
    /// Bumped whenever a translation is removed, so that
    /// processors flush the space's PCID before using it again.
    generation: AtomicU64,
    inner: SpinLock<Inner>,
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

fn free_frame(phys: u64) {
    if let Some(frames) = FRAMES.lock().as_mut() {
        frames.free(phys, 1);
    }
}

impl AddressSpace {
    /// An address space with no user mappings, belonging to `budget`. Its
    /// tables are charged to the budget when `charge` is set.
    pub fn new(budget: Arc<Budget>, charge: bool) -> Result<AddressSpace, Error> {
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
        let loaded = percpu::this().pcids.lock().current == self.id;
        let mut pages = Mapper::existing(&mut tables, inner.root, false);
        let mut removed = false;
        pages
            .unmap_present(virt, size, |v, phys| {
                if loaded {
                    cpu::invlpg(v);
                }
                removed = true;
                unmapped(phys);
            })
            .map_err(|_| Error::Internal)?;
        if removed {
            self.flush_elsewhere(loaded);
        }
        Ok(())
    }

    /// Makes processors that do not have the space loaded flush its PCID
    /// before using it again. When it is loaded here, its translations
    /// have already been invalidated.
    fn flush_elsewhere(&self, loaded: bool) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if !loaded {
            return;
        }
        let mut pcids = percpu::this().pcids.lock();
        if let Some(slot) = pcids.slots.iter_mut().find(|s| s.id == self.id) {
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
        let value = root | pcid | if flush { 0 } else { CR3_NO_FLUSH };
        // SAFETY: the tables share the kernel half; loading without the
        // no-flush bit drops the PCID's old translations.
        unsafe { cpu::write_cr3(value) };
        pcids.current = self.id;
    }
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
        let inner = self.inner.lock();
        let mut tables = Charged {
            budget: None,
            count: 0,
            refused: false,
        };
        let mut count = 1;
        Mapper::existing(&mut tables, inner.root, false).free_lower_half(|table| {
            free_frame(table);
            count += 1;
        });
        free_frame(inner.root);
        debug_assert!(inner.charged <= count);
        self.budget.release_kernel(inner.charged * PAGE_SIZE);
    }
}
