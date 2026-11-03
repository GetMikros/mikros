//! One Intel VT-d remapping unit: its registers, its root and context
//! tables, the second-level page tables of its domains and its queued
//! invalidation.

use crate::space::{self, map_device};
use kernel_core::nodes::{self, Map};
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::status::Error;

/// Registers, by offset.
const CAP: u64 = 0x08;
const ECAP: u64 = 0x10;
const GCMD: u64 = 0x18;
const GSTS: u64 = 0x1C;
const RTADDR: u64 = 0x20;
const IQT: u64 = 0x88;
const IQA: u64 = 0x90;

/// GCMD and GSTS bits.
const TE: u32 = 1 << 31;
const SRTP: u32 = 1 << 30;
const QIE: u32 = 1 << 26;
/// The GSTS bits that stay set until cleared, which a GCMD write must
/// repeat: translation, queued invalidation, interrupt remapping and
/// compatibility format interrupts.
const PERSISTENT: u32 = TE | QIE | 1 << 25 | 1 << 23;

/// ECAP bits: coherent table walks and queued invalidation.
const ECAP_C: u64 = 1 << 0;
const ECAP_QI: u64 = 1 << 1;

/// Second-level entry bits.
const READ: u64 = 1 << 0;
const WRITE: u64 = 1 << 1;
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;

/// Invalidation descriptors: context cache and IOTLB, global or for one
/// domain, and a wait that writes a status word.
const CONTEXT_GLOBAL: u64 = 0x1 | 1 << 4;
const IOTLB_DOMAIN: u64 = 0x2 | 2 << 4 | 1 << 6 | 1 << 7;
const IOTLB_GLOBAL: u64 = 0x2 | 1 << 4 | 1 << 6 | 1 << 7;
const WAIT: u64 = 0x5 | 1 << 5;

/// The descriptors the queue, one page of 16-byte entries, holds.
const QUEUE_ENTRIES: u64 = PAGE_SIZE / 16;

/// How often a register is polled before the unit counts as broken.
const POLLS: u32 = 10_000_000;

/// A zeroed frame, reached through the direct map.
fn table_frame() -> Result<u64, Error> {
    let phys = crate::memory::alloc_frame().ok_or(Error::NoMemory)?;
    // SAFETY: the frame was just allocated, and the direct map covers it.
    unsafe { core::ptr::write_bytes(space::direct(phys) as *mut u8, 0, PAGE_SIZE as usize) };
    Ok(phys)
}

/// The 64-bit table entry `index` of the table at `phys`.
fn entry(phys: u64, index: u64) -> *mut u64 {
    (space::direct(phys) + index * 8) as *mut u64
}

/// A requester ID's translation: its domain ID and the root of its
/// second-level tables, with how many pins each page has for reads and
/// for writes.
struct Domain {
    id: u16,
    root: u64,
    pages: Map<u64, (u32, u32)>,
}

pub struct Unit {
    /// The registers in the dynamic region.
    regs: u64,
    coherent: bool,
    /// The levels of the second-level tables, 3 or 4, and the address
    /// width field of context entries that selects them.
    levels: u32,
    width: u64,
    root: u64,
    /// The context table of each bus that has one.
    contexts: [u64; 256],
    domains: Map<u64, Domain>,
    next_domain: u16,
    max_domains: u32,
    queue: u64,
    tail: u64,
    /// The word that invalidation waits write.
    status: u64,
}

impl Unit {
    /// Sets up the unit whose registers are at `phys` with empty root
    /// tables and queued invalidation, without enabling translation.
    /// Fails with `NotSupported` when the unit lacks queued invalidation
    /// or second-level tables of 3 or 4 levels, and `TimedOut` when it
    /// does not complete a command.
    pub fn new(phys: u64) -> Result<Unit, Error> {
        crate::reserved::reserve_mmio(phys, PAGE_SIZE);
        let regs = map_device(phys, PAGE_SIZE);
        // SAFETY: the registers are mapped uncached, and only this code
        // uses them.
        let (cap, ecap) = unsafe {
            (
                ((regs + CAP) as *const u64).read_volatile(),
                ((regs + ECAP) as *const u64).read_volatile(),
            )
        };
        let sagaw = cap >> 8 & 0x1F;
        let (levels, width) = match () {
            () if sagaw & 0b100 != 0 => (4, 2),
            () if sagaw & 0b010 != 0 => (3, 1),
            () => return Err(Error::NotSupported),
        };
        if ecap & ECAP_QI == 0 {
            return Err(Error::NotSupported);
        }
        let mut unit = Unit {
            regs,
            coherent: ecap & ECAP_C != 0,
            levels,
            width,
            root: table_frame()?,
            contexts: [0; 256],
            domains: nodes::map(),
            next_domain: 1,
            max_domains: 1 << (4 + 2 * (cap & 0x7) as u32),
            queue: table_frame()?,
            tail: 0,
            status: table_frame()?,
        };
        // Queued invalidation is set up anew, as firmware may have used
        // it with its own queue; translation may stay on meanwhile.
        if unit.read32(GSTS) & QIE != 0 {
            unit.command(QIE, false)?;
        }
        unit.write64(IQT, 0);
        unit.write64(IQA, unit.queue);
        unit.command(QIE, true)?;
        // The new root table replaces any that firmware set, without
        // turning translation off.
        unit.write64(RTADDR, unit.root);
        unit.command(SRTP, true)?;
        unit.invalidate_all()?;
        Ok(unit)
    }

    fn read32(&self, offset: u64) -> u32 {
        // SAFETY: the registers are mapped uncached.
        unsafe { ((self.regs + offset) as *const u32).read_volatile() }
    }

    fn write64(&self, offset: u64, value: u64) {
        // SAFETY: the registers are mapped uncached, and the values are
        // tables and queues the unit owns.
        unsafe { ((self.regs + offset) as *mut u64).write_volatile(value) };
    }

    /// Sets or clears `bit` in the global command register, repeating the
    /// persistent bits, and waits until the status register shows it.
    fn command(&self, bit: u32, on: bool) -> Result<(), Error> {
        let status = self.read32(GSTS) & PERSISTENT;
        let value = if on { status | bit } else { status & !bit };
        // SAFETY: as in `write64`.
        unsafe { ((self.regs + GCMD) as *mut u32).write_volatile(value) };
        for _ in 0..POLLS {
            let shown = self.read32(GSTS) & bit != 0;
            // One-shot commands show completion by setting the bit.
            if shown == on {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::TimedOut)
    }

    /// Turns translation on.
    pub fn enable(&self) -> Result<(), Error> {
        self.command(TE, true)
    }

    /// Turns translation off, as when DMA is to go unrestricted.
    pub fn disable(&self) {
        let _ = self.command(TE, false);
    }

    /// Makes a table write visible to a unit whose walks do not snoop.
    fn flush(&self, at: *mut u64) {
        if !self.coherent {
            // SAFETY: CLFLUSH only writes the line back; MFENCE orders it.
            unsafe {
                core::arch::asm!("clflush [{}]", "mfence", in(reg) at, options(nostack));
            }
        }
    }

    /// Submits `descriptors` followed by a wait, and returns once the
    /// unit has written the wait's status.
    fn submit(&mut self, descriptors: &[[u64; 2]]) -> Result<(), Error> {
        let status = space::direct(self.status) as *mut u32;
        // SAFETY: the status word is the unit's own frame.
        unsafe { status.write_volatile(0) };
        let wait = [WAIT | 1 << 32, self.status];
        for d in descriptors.iter().chain([&wait]) {
            let at = space::direct(self.queue) + self.tail * 16;
            // SAFETY: the slot lies in the queue, which the unit has read
            // up to the tail, as each submission waits for its end.
            unsafe {
                (at as *mut u64).write_volatile(d[0]);
                ((at + 8) as *mut u64).write_volatile(d[1]);
            }
            self.flush(at as *mut u64);
            self.tail = (self.tail + 1) % QUEUE_ENTRIES;
        }
        self.write64(IQT, self.tail << 4);
        for _ in 0..POLLS {
            // SAFETY: as above; the unit writes the word.
            if unsafe { status.read_volatile() } == 1 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::TimedOut)
    }

    /// Invalidates every cached context entry and translation.
    pub fn invalidate_all(&mut self) -> Result<(), Error> {
        self.submit(&[[CONTEXT_GLOBAL, 0], [IOTLB_GLOBAL, 0]])
    }

    /// Invalidates the cached context entries and the translations of
    /// the domain of `rid`.
    fn invalidate(&mut self, rid: u64) -> Result<(), Error> {
        let Some(domain) = self.domains.get(&rid) else {
            return Ok(());
        };
        let iotlb = IOTLB_DOMAIN | u64::from(domain.id) << 16;
        self.submit(&[[CONTEXT_GLOBAL, 0], [iotlb, 0]])
    }

    /// The domain of `rid`, made with empty tables and a context entry on
    /// first use.
    fn domain(&mut self, rid: u64) -> Result<&mut Domain, Error> {
        if !self.domains.contains_key(&rid) {
            if u32::from(self.next_domain) >= self.max_domains {
                return Err(Error::NoResources);
            }
            let bus = (rid >> 8 & 0xFF) as usize;
            if self.contexts[bus] == 0 {
                let table = table_frame()?;
                let root = entry(self.root, 2 * bus as u64);
                // SAFETY: the root table is the unit's own frame; the
                // entry is written before the unit can use it.
                unsafe { root.write_volatile(table | 1) };
                self.flush(root);
                self.contexts[bus] = table;
            }
            let root = table_frame()?;
            // Insertions with a claim do not fail.
            let _claim = nodes::claim_insert()?;
            let id = self.next_domain;
            self.next_domain += 1;
            let devfn = rid & 0xFF;
            let low = entry(self.contexts[bus], 2 * devfn);
            let high = entry(self.contexts[bus], 2 * devfn + 1);
            // SAFETY: the context table is the unit's own frame, and the
            // high half is written before the present bit.
            unsafe {
                high.write_volatile(self.width | u64::from(id) << 8);
                low.write_volatile(root | 1);
            }
            self.flush(low);
            let domain = Domain {
                id,
                root,
                pages: nodes::map(),
            };
            self.domains.insert(rid, domain);
        }
        Ok(self.domains.get_mut(&rid).expect("just made"))
    }

    /// The leaf entry of `addr` in the tables from `root`, made with
    /// intermediate tables when `make`.
    fn leaf(&self, root: u64, addr: u64, make: bool) -> Result<Option<*mut u64>, Error> {
        let mut table = root;
        for level in (1..self.levels).rev() {
            let at = entry(table, addr >> (12 + 9 * level) & 0x1FF);
            // SAFETY: the tables are the unit's own frames.
            let value = unsafe { at.read_volatile() };
            table = if value & (READ | WRITE) != 0 {
                value & ADDRESS
            } else if make {
                let next = table_frame()?;
                // SAFETY: as above.
                unsafe { at.write_volatile(next | READ | WRITE) };
                self.flush(at);
                next
            } else {
                return Ok(None);
            };
        }
        Ok(Some(entry(table, addr >> 12 & 0x1FF)))
    }

    /// Lets the device `rid` read, write or both the page at `phys`, at
    /// that address, counting the pin.
    pub fn map(&mut self, rid: u64, phys: u64, read: bool, write: bool) -> Result<(), Error> {
        let width = 12 + 9 * self.levels;
        if phys >> width != 0 {
            return Err(Error::OutOfRange);
        }
        let _claim = nodes::claim_insert()?;
        let root = self.domain(rid)?.root;
        let leaf = self.leaf(root, phys, true)?.expect("made");
        let domain = self.domains.get_mut(&rid).expect("made");
        let (reads, writes) = domain.pages.get(&phys).copied().unwrap_or((0, 0));
        let counts = (reads + u32::from(read), writes + u32::from(write));
        domain.pages.insert(phys, counts);
        self.set_leaf(leaf, phys, counts);
        Ok(())
    }

    /// Takes back one pin of the page at `phys` from the device `rid`.
    pub fn unmap(&mut self, rid: u64, phys: u64, read: bool, write: bool) {
        let Some(domain) = self.domains.get_mut(&rid) else {
            return;
        };
        let Some(&(reads, writes)) = domain.pages.get(&phys) else {
            return;
        };
        let counts = (reads - u32::from(read), writes - u32::from(write));
        if counts == (0, 0) {
            domain.pages.remove(&phys);
        } else if let Some(c) = domain.pages.get_mut(&phys) {
            *c = counts;
        }
        let root = domain.root;
        if let Ok(Some(leaf)) = self.leaf(root, phys, false) {
            self.set_leaf(leaf, phys, counts);
        }
    }

    fn set_leaf(&self, leaf: *mut u64, phys: u64, (reads, writes): (u32, u32)) {
        let mut value = phys;
        if reads > 0 {
            value |= READ;
        }
        if writes > 0 {
            value |= WRITE;
        }
        if value & (READ | WRITE) == 0 {
            value = 0;
        }
        // SAFETY: the leaf is in the unit's own tables.
        unsafe { leaf.write_volatile(value) };
        self.flush(leaf);
    }

    /// Makes the changes to the domain of `rid` take effect, and returns
    /// once the unit can no longer use what was removed.
    pub fn sync(&mut self, rid: u64) -> Result<(), Error> {
        self.invalidate(rid)
    }
}
