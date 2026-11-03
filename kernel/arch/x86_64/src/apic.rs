//! The interrupt controllers: the legacy PICs, the I/O APICs and the
//! local APIC.

use crate::cpu::{out8, read_msr, write_msr};
use crate::log::log;
use crate::memory::DirectMem;
use crate::space::map_device;
use alloc::vec::Vec;
use boot_protocol::BootInfo;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use kernel_core::acpi::Tables;
use kernel_core::acpi::madt::{self, Entry};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The first vector of the legacy PICs.
pub const PIC_BASE: u8 = 32;
/// The vectors of device interrupts.
pub const DEVICE_VECTORS: core::ops::Range<u8> = 48..240;
/// The local APIC timer vector.
pub const TIMER_VECTOR: u8 = 240;
/// The vector of the IOMMUs' fault events, the first of those reserved
/// for the kernel.
pub const IOMMU_FAULT_VECTOR: u8 = 241;
/// The local APIC spurious interrupt vector.
pub const SPURIOUS_VECTOR: u8 = 255;

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_ENABLE: u64 = 1 << 11;
const APIC_X2: u64 = 1 << 10;

/// Local APIC registers, by their xAPIC offset.
const ID: u32 = 0x20;
const VERSION: u32 = 0x30;
const EOI: u32 = 0xB0;
const SPURIOUS: u32 = 0xF0;
const LVT_CMCI: u32 = 0x2F0;
pub const LVT_TIMER: u32 = 0x320;
const LVT_THERMAL: u32 = 0x330;
const LVT_PERF: u32 = 0x340;
const LVT_LINT0: u32 = 0x350;
const LVT_LINT1: u32 = 0x360;
const LVT_ERROR: u32 = 0x370;

const LVT_MASKED: u32 = 1 << 16;
const LVT_NMI: u32 = 0b100 << 8;
const LVT_ACTIVE_LOW: u32 = 1 << 13;
const LVT_LEVEL: u32 = 1 << 15;
const SPURIOUS_ENABLE: u32 = 1 << 8;

static X2APIC: AtomicBool = AtomicBool::new(false);
/// The local APIC ID of the boot processor.
static BOOT_ID: AtomicU32 = AtomicU32::new(0);
/// The xAPIC registers in the dynamic region.
static XAPIC: AtomicU64 = AtomicU64::new(0);

/// Reads the local APIC register at xAPIC offset `register`.
pub fn read(register: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: the register exists in x2APIC mode, which is enabled,
        // and reading it has no side effects.
        unsafe { read_msr(0x800 + (register >> 4)) as u32 }
    } else {
        let base = XAPIC.load(Ordering::Relaxed);
        // SAFETY: `base` maps the local APIC registers uncached.
        unsafe { ((base + u64::from(register)) as *const u32).read_volatile() }
    }
}

/// Writes the local APIC register at xAPIC offset `register`.
pub fn write(register: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: the register exists in x2APIC mode, and callers write
        // only values the kernel's setup allows.
        unsafe { write_msr(0x800 + (register >> 4), u64::from(value)) };
    } else {
        let base = XAPIC.load(Ordering::Relaxed);
        // SAFETY: as above, through the uncached mapping.
        unsafe { ((base + u64::from(register)) as *mut u32).write_volatile(value) };
    }
}

/// Whether the local APIC is in x2APIC mode.
pub fn x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

/// The local APIC ID of the boot processor.
pub fn boot_id() -> u32 {
    BOOT_ID.load(Ordering::Relaxed)
}

/// Signals end of interrupt to the local APIC.
pub fn eoi() {
    write(EOI, 0);
}

/// The local APIC ID of this processor.
pub fn id() -> u32 {
    let id = read(ID);
    if X2APIC.load(Ordering::Relaxed) {
        id
    } else {
        id >> 24
    }
}

/// Moves the PICs to vectors 32 to 47 and masks all their lines.
fn mask_pics() {
    // SAFETY: ports 0x20, 0x21, 0xA0 and 0xA1 are the two PICs, which
    // nothing else in the kernel uses; port 0x80 is the POST port, written
    // only to wait between PIC writes.
    unsafe {
        let wait = || out8(0x80, 0);
        for (command, data, vector, wiring) in [
            (0x20, 0x21, PIC_BASE, 1 << 2),
            (0xA0, 0xA1, PIC_BASE + 8, 2),
        ] {
            out8(command, 0x11);
            wait();
            out8(data, vector);
            wait();
            out8(data, wiring);
            wait();
            out8(data, 0x01);
            wait();
            out8(data, 0xFF);
        }
    }
}

/// An I/O APIC: the first global system interrupt it handles, how many
/// it handles, and its select register in the dynamic region, with the
/// window register 16 bytes above it.
struct IoApic {
    id: u8,
    gsi_base: u32,
    entries: u32,
    base: u64,
}

impl IoApic {
    /// Maps the I/O APIC with ID `id` at `phys`, which handles the global
    /// system interrupts from `gsi_base` on.
    fn map(id: u8, phys: u64, gsi_base: u32) -> IoApic {
        crate::reserved::reserve_mmio(phys, 0x20);
        let mut apic = IoApic {
            id,
            gsi_base,
            entries: 0,
            base: map_device(phys, 0x20),
        };
        apic.entries = (apic.read(1) >> 16 & 0xFF) + 1;
        apic
    }

    fn read(&self, index: u32) -> u32 {
        // SAFETY: `base` maps the I/O APIC's select and window registers
        // uncached, and the caller holds the lock of `IO_APICS` or runs
        // before anything else uses it.
        unsafe {
            (self.base as *mut u32).write_volatile(index);
            ((self.base + 0x10) as *const u32).read_volatile()
        }
    }

    fn write(&self, index: u32, value: u32) {
        // SAFETY: as in `read`; the values written are redirection entries
        // the kernel chose.
        unsafe {
            (self.base as *mut u32).write_volatile(index);
            ((self.base + 0x10) as *mut u32).write_volatile(value);
        }
    }

    /// The register index of the low half of the redirection entry of
    /// `gsi`, if this I/O APIC handles it.
    fn entry(&self, gsi: u32) -> Option<u32> {
        let n = gsi.checked_sub(self.gsi_base)?;
        (n < self.entries).then_some(0x10 + 2 * n)
    }
}

/// The I/O APICs that the MADT lists.
static IO_APICS: SpinLock<Vec<IoApic>> = SpinLock::new(Vec::new());

/// Points the redirection entry of global system interrupt `gsi` at
/// `vector` on the boot processor with fixed delivery, level or edge
/// triggered and active low or high, and unmasks it. When interrupts are
/// remapped, the entry instead refers to remapping entry `index`, which
/// gets the vector, destination and trigger mode and accepts the
/// interrupt only from the I/O APIC's source. Fails with `NotFound` when
/// no I/O APIC handles `gsi`, and when remapping, with `NotSupported` when
/// the DMAR table gives no source for that I/O APIC.
pub fn route(gsi: u32, vector: u8, index: u16, level: bool, active_low: bool) -> Result<(), Error> {
    let (id, remapped) = {
        let apics = IO_APICS.lock();
        let apic = apics
            .iter()
            .find(|a| a.entry(gsi).is_some())
            .ok_or(Error::NotFound)?;
        (apic.id, crate::iommu::remapping())
    };
    if remapped {
        let source = crate::iommu::io_apic_source(id).ok_or(Error::NotSupported)?;
        crate::iommu::remap(index, Some((vector, level, source)))?;
    }
    let apics = IO_APICS.lock();
    let (apic, entry) = apics
        .iter()
        .find_map(|a| Some((a, a.entry(gsi)?)))
        .ok_or(Error::NotFound)?;
    let mut low = u32::from(vector);
    if level {
        low |= LVT_LEVEL;
    }
    if active_low {
        low |= LVT_ACTIVE_LOW;
    }
    let high = if remapped {
        // The remappable format: the index, with its top bit in the low
        // half, and the format bit.
        low |= u32::from(index >> 15) << 11;
        u32::from(index & 0x7FFF) << 17 | 1 << 16
    } else {
        // Physical destination mode, with the boot processor's ID in the
        // top byte.
        boot_id() << 24
    };
    // The entry stays masked until its destination is set.
    apic.write(entry, LVT_MASKED);
    apic.write(entry + 1, high);
    apic.write(entry, low);
    Ok(())
}

/// Masks or unmasks the redirection entry of `gsi`, which `route` set.
pub fn mask_line(gsi: u32, masked: bool) {
    let apics = IO_APICS.lock();
    let Some((apic, index)) = apics.iter().find_map(|a| Some((a, a.entry(gsi)?))) else {
        return;
    };
    let low = apic.read(index);
    let low = if masked {
        low | LVT_MASKED
    } else {
        low & !LVT_MASKED
    };
    apic.write(index, low);
}

/// Sets up the local APIC with every local interrupt masked except the
/// NMI inputs the MADT names.
fn init_local(madt: &[u8]) {
    let x2apic = core::arch::x86_64::__cpuid(1).ecx & (1 << 21) != 0;
    // SAFETY: IA32_APIC_BASE exists on every processor with an APIC,
    // which the feature check requires. The enable bit is set before the
    // x2APIC bit, as the processor requires.
    unsafe {
        let base = read_msr(IA32_APIC_BASE);
        write_msr(IA32_APIC_BASE, base | APIC_ENABLE);
        if x2apic {
            write_msr(IA32_APIC_BASE, base | APIC_ENABLE | APIC_X2);
        } else {
            crate::reserved::reserve_mmio(base & 0x000F_FFFF_FFFF_F000, 0x1000);
            XAPIC.store(
                map_device(base & 0x000F_FFFF_FFFF_F000, 0x1000),
                Ordering::Relaxed,
            );
        }
    }
    X2APIC.store(x2apic, Ordering::Relaxed);

    let max_lvt = read(VERSION) >> 16 & 0xFF;
    // Entries beyond the first four exist only when the version register
    // counts them.
    let optional = [(4, LVT_PERF), (5, LVT_THERMAL), (6, LVT_CMCI)]
        .into_iter()
        .filter(|&(needed, _)| max_lvt >= needed)
        .map(|(_, lvt)| lvt);
    for lvt in [LVT_TIMER, LVT_LINT0, LVT_LINT1, LVT_ERROR]
        .into_iter()
        .chain(optional)
    {
        let vector = if lvt == LVT_TIMER {
            u32::from(TIMER_VECTOR)
        } else {
            0
        };
        write(lvt, LVT_MASKED | vector);
    }
    for (lint, flags) in madt::nmi_inputs(madt, id()) {
        let lvt = match lint {
            0 => LVT_LINT0,
            1 => LVT_LINT1,
            _ => continue,
        };
        let polarity = if madt::active_low(flags) {
            LVT_ACTIVE_LOW
        } else {
            0
        };
        write(lvt, LVT_NMI | polarity);
    }
    write(SPURIOUS, SPURIOUS_ENABLE | u32::from(SPURIOUS_VECTOR));
}

/// Masks the legacy PICs and every I/O APIC entry, and sets up the local
/// APIC. Interrupts must be disabled.
pub fn init(info: &BootInfo) {
    let mem = DirectMem(info);
    let madt = Tables::new(&mem, info.firmware_table_phys)
        .and_then(|t| t.find(*b"APIC"))
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("the ACPI MADT is missing"));
    mask_pics();
    let mut apics = IO_APICS.lock();
    for e in madt::entries(madt) {
        if let Entry::IoApic {
            id,
            address,
            gsi_base,
        } = e
        {
            let apic = IoApic::map(id, u64::from(address), gsi_base);
            for n in 0..apic.entries {
                let index = 0x10 + 2 * n;
                apic.write(index, apic.read(index) | LVT_MASKED);
            }
            apics
                .try_reserve(1)
                .unwrap_or_else(|_| panic!("no memory for the I/O APICs"));
            apics.push(apic);
        }
    }
    let io_apics = apics.len();
    drop(apics);
    init_local(madt);
    BOOT_ID.store(id(), Ordering::Relaxed);
    let mode = if X2APIC.load(Ordering::Relaxed) {
        "x2APIC"
    } else {
        "xAPIC"
    };
    log!(
        Info,
        "local APIC {} in {mode} mode, {io_apics} I/O APICs",
        id()
    );
}
