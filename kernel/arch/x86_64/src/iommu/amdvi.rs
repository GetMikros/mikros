//! One AMD-Vi IOMMU: its registers, its device table, the page tables of
//! its domains, its interrupt remapping tables, its command buffer and its
//! event log.

use crate::space::{self, map_device};
use kernel_core::nodes::{self, Map};
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::status::Error;

/// Registers, by offset.
const DEVTAB_BASE: u64 = 0x0000;
const CMD_BASE: u64 = 0x0008;
const EVENT_BASE: u64 = 0x0010;
const CONTROL: u64 = 0x0018;
const EFR: u64 = 0x0030;
const CMD_HEAD: u64 = 0x2000;
const CMD_TAIL: u64 = 0x2008;
const EVENT_HEAD: u64 = 0x2010;
const EVENT_TAIL: u64 = 0x2018;
const STATUS: u64 = 0x2020;
/// The size of the register space.
const REGISTERS: u64 = 0x4000;

/// Control bits.
const IOMMU_EN: u64 = 1 << 0;
const EVENT_LOG_EN: u64 = 1 << 2;
const EVENT_INT_EN: u64 = 1 << 3;
const CMD_BUF_EN: u64 = 1 << 12;
/// Status bits, cleared by writing them: the event log overflowed, and an
/// event interrupt is pending.
const EVENT_OVERFLOW: u64 = 1 << 0;
const EVENT_LOG_INT: u64 = 1 << 1;
/// EFR: INVALIDATE_IOMMU_ALL is supported.
const IA_SUP: u64 = 1 << 6;

/// The device table: one 32-byte entry per device ID, 2 MiB.
const DEVICES: u64 = 0x1_0000;
const DTE_SIZE: u64 = 32;
/// Device table entry bits: valid, translation valid, the paging mode
/// field, and read and write permission; the interrupt map valid bit, the
/// table length field and remapped interrupt control in the third quad.
const DTE_V: u64 = 1 << 0;
const DTE_TV: u64 = 1 << 1;
const DTE_IR: u64 = 1 << 61;
const DTE_IW: u64 = 1 << 62;
const DTE_IV: u64 = 1 << 0;
const DTE_REMAPPED: u64 = 0b10 << 60;
/// Interrupt tables of 256 32-bit entries, one per vector.
const INT_TABLE_LEN: u64 = 8;

/// Page table entry bits: present, the next level field, and read and
/// write permission.
const PR: u64 = 1 << 0;
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
/// The levels of the page tables the kernel builds.
const LEVELS: u32 = 4;

/// Command opcodes, in bits 60 to 63.
const COMPLETION_WAIT: u64 = 0x1 << 60;
const INVALIDATE_DEVTAB_ENTRY: u64 = 0x2 << 60;
const INVALIDATE_IOMMU_PAGES: u64 = 0x3 << 60;
const INVALIDATE_INTERRUPT_TABLE: u64 = 0x5 << 60;
const INVALIDATE_IOMMU_ALL: u64 = 0x8 << 60;
/// The command buffer and the event log: one page of 16-byte entries,
/// given as a power of two in bits 56 to 59 of their base registers.
const RING_ENTRIES: u64 = PAGE_SIZE / 16;
const RING_LEN: u64 = 8 << 56;

/// The event code of a page fault, and its write flag.
const IO_PAGE_FAULT: u64 = 0x2;
const FAULT_WRITE: u64 = 1 << 53;

/// How often the completion word is polled before the unit counts as
/// broken.
const POLLS: u32 = 10_000_000;

/// A zeroed frame, reached through the direct map.
fn table_frame() -> Result<u64, Error> {
    let phys = crate::memory::alloc_frame().ok_or(Error::NoMemory)?;
    // SAFETY: the frame was just allocated, and the direct map covers it.
    unsafe { core::ptr::write_bytes(space::direct(phys) as *mut u8, 0, PAGE_SIZE as usize) };
    Ok(phys)
}

fn entry(phys: u64, index: u64) -> *mut u64 {
    (space::direct(phys) + index * 8) as *mut u64
}

/// A device's translation: its domain ID and the root of its page
/// tables, with how many pins each page has for reads and for writes.
struct Domain {
    id: u16,
    root: u64,
    pages: Map<u64, (u32, u32)>,
}

pub struct Unit {
    regs: u64,
    devtab: u64,
    cmd: u64,
    cmd_tail: u64,
    events: u64,
    status: u64,
    efr: u64,
    domains: Map<u16, Domain>,
    next_domain: u16,
    /// The interrupt remapping table of each device that has one.
    int_tables: Map<u16, u64>,
    remapping: bool,
}

impl Unit {
    /// Sets up the IOMMU whose registers are at `phys` with a device
    /// table that blocks the DMA of every device, a command buffer and an
    /// event log, and turns it on.
    pub fn new(phys: u64) -> Result<Unit, Error> {
        crate::reserved::reserve_mmio(phys, REGISTERS);
        let regs = map_device(phys, REGISTERS);
        let devtab = crate::memory::alloc_contiguous(DEVICES * DTE_SIZE / PAGE_SIZE, 1, u64::MAX)
            .ok_or(Error::NoMemory)?;
        let blocked = [DTE_V | DTE_TV, 0, 0, 0];
        for device in 0..DEVICES {
            let at = space::direct(devtab) + device * DTE_SIZE;
            // SAFETY: the table was just allocated for the unit.
            unsafe { *(at as *mut [u64; 4]) = blocked };
        }
        let mut unit = Unit {
            regs,
            devtab,
            cmd: table_frame()?,
            cmd_tail: 0,
            events: table_frame()?,
            status: table_frame()?,
            efr: 0,
            domains: nodes::map(),
            next_domain: 1,
            int_tables: nodes::map(),
            remapping: false,
        };
        unit.efr = unit.read(EFR);
        // The unit is set up anew, as firmware may have used it.
        unit.write(
            CONTROL,
            unit.read(CONTROL) & !(IOMMU_EN | CMD_BUF_EN | EVENT_LOG_EN),
        );
        unit.write(DEVTAB_BASE, devtab | (DEVICES * DTE_SIZE / PAGE_SIZE - 1));
        unit.write(CMD_BASE, unit.cmd | RING_LEN);
        unit.write(EVENT_BASE, unit.events | RING_LEN);
        for reg in [CMD_HEAD, CMD_TAIL, EVENT_HEAD, EVENT_TAIL] {
            unit.write(reg, 0);
        }
        unit.write(STATUS, EVENT_OVERFLOW | EVENT_LOG_INT);
        let control = unit.read(CONTROL) | CMD_BUF_EN | EVENT_LOG_EN | EVENT_INT_EN | IOMMU_EN;
        unit.write(CONTROL, control);
        unit.invalidate_all()?;
        Ok(unit)
    }

    fn read(&self, offset: u64) -> u64 {
        // SAFETY: the registers are mapped uncached, and only this code
        // uses them.
        unsafe { ((self.regs + offset) as *const u64).read_volatile() }
    }

    fn write(&self, offset: u64, value: u64) {
        // SAFETY: as in `read`; the values are tables and rings the unit
        // owns.
        unsafe { ((self.regs + offset) as *mut u64).write_volatile(value) };
    }

    /// Turns the unit off, which leaves DMA unrestricted.
    pub fn disable(&self) {
        self.write(CONTROL, self.read(CONTROL) & !IOMMU_EN);
    }

    /// Submits `commands` followed by a completion wait, and returns once
    /// the unit has written the wait's word.
    fn submit(&mut self, commands: &[[u64; 2]]) -> Result<(), Error> {
        let word = space::direct(self.status) as *mut u64;
        // SAFETY: the word is the unit's own frame.
        unsafe { word.write_volatile(0) };
        // Store the value 1 at the word's address.
        let wait = [COMPLETION_WAIT | self.status | 1, 1];
        for c in commands.iter().chain([&wait]) {
            let at = space::direct(self.cmd) + self.cmd_tail * 16;
            // SAFETY: the slot lies in the buffer, which the unit has read
            // up to the tail, as each submission waits for its end.
            unsafe { *(at as *mut [u64; 2]) = *c };
            self.cmd_tail = (self.cmd_tail + 1) % RING_ENTRIES;
        }
        self.write(CMD_TAIL, self.cmd_tail * 16);
        for _ in 0..POLLS {
            // SAFETY: as above; the unit writes the word.
            if unsafe { word.read_volatile() } == 1 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::TimedOut)
    }

    /// Invalidates everything the unit may have cached.
    fn invalidate_all(&mut self) -> Result<(), Error> {
        if self.efr & IA_SUP != 0 {
            return self.submit(&[[INVALIDATE_IOMMU_ALL, 0]]);
        }
        for first in (0..DEVICES).step_by(64) {
            let mut commands = [[0u64; 2]; 128];
            for (i, device) in (first..first + 64).enumerate() {
                commands[2 * i] = [INVALIDATE_DEVTAB_ENTRY | device, 0];
                commands[2 * i + 1] = [INVALIDATE_INTERRUPT_TABLE | device, 0];
            }
            self.submit(&commands)?;
        }
        Ok(())
    }

    /// The device table entry of `device`, as four quads.
    fn dte(&self, device: u16) -> *mut u64 {
        (space::direct(self.devtab) + u64::from(device) * DTE_SIZE) as *mut u64
    }

    /// In the diagnostic build, calls `each` with the device, ID and
    /// pinned pages of each domain.
    #[cfg(feature = "diag")]
    pub fn domains(
        &self,
        each: &mut dyn FnMut(u64, u16, usize) -> core::fmt::Result,
    ) -> core::fmt::Result {
        self.domains
            .iter()
            .try_for_each(|(device, d)| each(u64::from(*device), d.id, d.pages.len()))
    }

    /// The domain of `device`, made with empty tables and a device table
    /// entry on first use.
    fn domain(&mut self, device: u16) -> Result<&mut Domain, Error> {
        if !self.domains.contains_key(&device) {
            if self.next_domain == u16::MAX {
                return Err(Error::NoResources);
            }
            let root = table_frame()?;
            // Insertions with a claim do not fail.
            let _claim = nodes::claim_insert()?;
            let id = self.next_domain;
            self.next_domain += 1;
            let dte = self.dte(device);
            // SAFETY: the entry lies in the unit's device table; the
            // domain ID is written before the entry points at the tables,
            // and the unit rereads it after the invalidation below.
            unsafe {
                dte.add(1).write_volatile(u64::from(id));
                let mode = u64::from(LEVELS) << 9;
                dte.write_volatile(DTE_V | DTE_TV | mode | root | DTE_IR | DTE_IW);
            }
            self.domains.insert(
                device,
                Domain {
                    id,
                    root,
                    pages: nodes::map(),
                },
            );
            crate::log::diag!("AMD-Vi: device {device:#06x} is translated in domain {id}");
        }
        Ok(self.domains.get_mut(&device).expect("just made"))
    }

    /// The leaf entry of `addr` in the tables from `root`, made with
    /// intermediate tables when `make`.
    fn leaf(root: u64, addr: u64, make: bool) -> Result<Option<*mut u64>, Error> {
        let mut table = root;
        for level in (1..LEVELS).rev() {
            let at = entry(table, addr >> (12 + 9 * level) & 0x1FF);
            // SAFETY: the tables are the unit's own frames.
            let value = unsafe { at.read_volatile() };
            table = if value & PR != 0 {
                value & ADDRESS
            } else if make {
                let next = table_frame()?;
                let next_level = u64::from(level) << 9;
                // SAFETY: as above.
                unsafe { at.write_volatile(next | next_level | PR | DTE_IR | DTE_IW) };
                next
            } else {
                return Ok(None);
            };
        }
        Ok(Some(entry(table, addr >> 12 & 0x1FF)))
    }

    fn set_leaf(leaf: *mut u64, phys: u64, (reads, writes): (u32, u32)) {
        let mut value = 0;
        if reads > 0 {
            value |= DTE_IR;
        }
        if writes > 0 {
            value |= DTE_IW;
        }
        if value != 0 {
            value |= phys | PR;
        }
        // SAFETY: the leaf is in the unit's own tables.
        unsafe { leaf.write_volatile(value) };
    }

    /// Lets `device` read, write or both the page at `phys`, at that
    /// address, counting the pin.
    pub fn map(&mut self, device: u16, phys: u64, read: bool, write: bool) -> Result<(), Error> {
        if phys >> (12 + 9 * LEVELS) != 0 {
            return Err(Error::OutOfRange);
        }
        let _claim = nodes::claim_insert()?;
        let root = self.domain(device)?.root;
        let leaf = Self::leaf(root, phys, true)?.expect("made");
        let domain = self.domains.get_mut(&device).expect("made");
        let (reads, writes) = domain.pages.get(&phys).copied().unwrap_or((0, 0));
        let counts = (reads + u32::from(read), writes + u32::from(write));
        domain.pages.insert(phys, counts);
        Self::set_leaf(leaf, phys, counts);
        Ok(())
    }

    /// Takes back one pin of the page at `phys` from `device`.
    pub fn unmap(&mut self, device: u16, phys: u64, read: bool, write: bool) {
        let Some(domain) = self.domains.get_mut(&device) else {
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
        if let Ok(Some(leaf)) = Self::leaf(domain.root, phys, false) {
            Self::set_leaf(leaf, phys, counts);
        }
    }

    /// Makes the changes to the domain of `device` take effect, and
    /// returns once the unit no longer uses what was removed.
    pub fn sync(&mut self, device: u16) -> Result<(), Error> {
        let Some(domain) = self.domains.get(&device) else {
            return Ok(());
        };
        // Every page of the domain, with the page directory caches.
        let pages = INVALIDATE_IOMMU_PAGES | u64::from(domain.id) << 32;
        let all = 0x7FFF_FFFF_FFFF_F000 | 1 << 1 | 1;
        self.submit(&[
            [INVALIDATE_DEVTAB_ENTRY | u64::from(device), 0],
            [pages, all],
        ])
    }

    /// Makes every device's interrupts go through a remapping table, which
    /// blocks them until an entry is written.
    pub fn enable_remapping(&mut self) -> Result<(), Error> {
        for device in 0..DEVICES {
            // SAFETY: the entry lies in the unit's device table; with no
            // table, remapped interrupts are blocked.
            unsafe { self.dte(device as u16).add(2).write_volatile(DTE_IV) };
        }
        self.remapping = true;
        self.invalidate_all()
    }

    /// Writes entry `vector` of the interrupt table of `device`, made on
    /// first use, and waits until the unit no longer uses what it held.
    pub fn set_remap(&mut self, device: u16, vector: u8, entry: u32) -> Result<(), Error> {
        if !self.remapping {
            return Ok(());
        }
        let table = match self.int_tables.get(&device) {
            Some(&table) => table,
            None => {
                let table = table_frame()?;
                let _claim = nodes::claim_insert()?;
                self.int_tables.insert(device, table);
                let control = DTE_IV | INT_TABLE_LEN << 1 | table | DTE_REMAPPED;
                // SAFETY: the entry lies in the unit's device table.
                unsafe { self.dte(device).add(2).write_volatile(control) };
                table
            }
        };
        let at = (space::direct(table) + u64::from(vector) * 4) as *mut u32;
        // SAFETY: the entry lies in the device's table.
        unsafe { at.write_volatile(entry) };
        let device = u64::from(device);
        self.submit(&[
            [INVALIDATE_DEVTAB_ENTRY | device, 0],
            [INVALIDATE_INTERRUPT_TABLE | device, 0],
        ])
    }

    /// Passes each page fault in the event log, as the device ID, the
    /// address and whether it was a write, to `report`, and empties the
    /// log.
    pub fn take_faults(&self, mut report: impl FnMut(u16, u64, bool)) {
        let tail = self.read(EVENT_TAIL);
        let mut head = self.read(EVENT_HEAD);
        while head != tail {
            let at = space::direct(self.events) + head;
            // SAFETY: the unit wrote the entries up to the tail.
            let [low, high] = unsafe { *(at as *const [u64; 2]) };
            if low >> 60 == IO_PAGE_FAULT {
                report(low as u16, high & !0xFFF, low & FAULT_WRITE != 0);
            } else {
                // Other events, such as command errors that stop the
                // command processor, are recorded as they are.
                crate::log::log!(
                    Error,
                    "AMD-Vi: event {:#x}: {low:#018x} {high:#018x}",
                    low >> 60
                );
            }
            head = (head + 16) % (RING_ENTRIES * 16);
        }
        if self.read(STATUS) & EVENT_OVERFLOW != 0 {
            crate::log::log!(Error, "AMD-Vi: the event log overflowed");
        }
        self.write(EVENT_HEAD, head);
        self.write(STATUS, EVENT_OVERFLOW | EVENT_LOG_INT);
    }
}
