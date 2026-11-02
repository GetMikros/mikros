//! Setting up and reading the monotonic clock.

use crate::clock::{PIT_HZ, PM_TIMER_HZ, TscLeaves, clock_page, measured_hz, scaled_ns, tsc_scale};
use crate::cpu::{in8, in32, out8, rdtsc};
use crate::log::log;
use crate::memory::DirectMem;
use crate::memory_object::MemoryObject;
use alloc::sync::Arc;
use boot_protocol::BootInfo;
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use kernel_core::acpi::{self, Address, PmTimer, Tables};
use kernel_core::clock::{Extender, ticks_to_ns};
use kernel_core::object::budget::Budget;
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::object::resource::KIND_IOPORT;
use kernel_core::status::Error;

const NONE: u8 = 0;
const TSC: u8 = 1;
const HPET: u8 = 2;
const PM: u8 = 3;

/// HPET registers.
const HPET_CAPABILITIES: u64 = 0x00;
const HPET_CONFIG: u64 = 0x10;
const HPET_COUNTER: u64 = 0xF0;
const HPET_SIZE: u64 = 0x400;
const HPET_ENABLE: u64 = 1;
const HPET_64BIT: u64 = 1 << 13;

static SOURCE: AtomicU8 = AtomicU8::new(NONE);
/// The counter frequency, or for the HPET its period in femtoseconds.
/// For the TSC, the factor and shift of `tsc_scale` instead.
static RATE: AtomicU64 = AtomicU64::new(0);
/// The extended counter value at setup, subtracted from later values.
static BASE: AtomicU64 = AtomicU64::new(0);
/// The HPET's registers in the dynamic region, or the PM timer's I/O port
/// or mapped address.
static ADDRESS: AtomicU64 = AtomicU64::new(0);
static PM_IN_MEMORY: AtomicU8 = AtomicU8::new(0);
static EXTEND_24: Extender = Extender::new(24);
static EXTEND_32: Extender = Extender::new(32);
static EXTEND_64: Extender = Extender::new(64);
static WIDTH: AtomicU8 = AtomicU8::new(64);
/// The TSC frequency.
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

/// The TSC frequency, once the clock is set up.
pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

fn extender() -> &'static Extender {
    match WIDTH.load(Ordering::Relaxed) {
        24 => &EXTEND_24,
        32 => &EXTEND_32,
        _ => &EXTEND_64,
    }
}

fn hpet_read(virt: u64, register: u64) -> u64 {
    // SAFETY: `virt` maps the HPET's register block uncached, and reading
    // its registers has no side effects.
    unsafe { ((virt + register) as *const u64).read_volatile() }
}

fn pm_read() -> u64 {
    let address = ADDRESS.load(Ordering::Relaxed);
    if PM_IN_MEMORY.load(Ordering::Relaxed) != 0 {
        // SAFETY: the address maps the PM timer register uncached.
        u64::from(unsafe { (address as *const u32).read_volatile() })
    } else {
        // SAFETY: the FADT names this port as the PM timer, which only
        // reports its count.
        u64::from(unsafe { in32(address as u16) })
    }
}

/// The current raw counter value of the source, extended to 64 bits.
fn counter() -> u64 {
    match SOURCE.load(Ordering::Relaxed) {
        TSC => rdtsc(),
        HPET => extender().extend(hpet_read(ADDRESS.load(Ordering::Relaxed), HPET_COUNTER)),
        PM => extender().extend(pm_read()),
        _ => 0,
    }
}

/// The monotonic clock in nanoseconds, 0 before it is set up.
pub fn now() -> i64 {
    let source = SOURCE.load(Ordering::Acquire);
    let ticks = counter().saturating_sub(BASE.load(Ordering::Relaxed));
    let rate = RATE.load(Ordering::Relaxed);
    match source {
        TSC => scaled_ns(ticks, rate as u32, (rate >> 32) as u32),
        HPET => i64::try_from(u128::from(ticks) * u128::from(rate) / 1_000_000).unwrap_or(i64::MAX),
        NONE => 0,
        _ => ticks_to_ns(ticks, rate),
    }
}

/// Measures the frequency of `counter` while `reference`, running at
/// `hz`, advances by about 10 ms.
fn measure(counter: &dyn Fn() -> u64, mut reference: impl FnMut() -> u64, hz: u64) -> u64 {
    let wait = hz / 100;
    let start = reference();
    let counter_start = counter();
    let mut now = start;
    while now - start < wait {
        now = reference();
    }
    measured_hz(counter() - counter_start, now - start, hz)
}

/// Measures the frequency of `counter` against PIT channel 2, counting
/// down from the count for 10 ms in mode 0 with the gate open and the
/// speaker off.
fn measure_with_pit(counter: &dyn Fn() -> u64) -> Option<u64> {
    let count = PIT_HZ / 100;
    // SAFETY: ports 0x61, 0x42 and 0x43 are the PIT channel 2 gate and
    // registers, which nothing else in the kernel uses.
    unsafe {
        let gate = in8(0x61);
        out8(0x61, (gate & !0x02) | 0x01);
        out8(0x43, 0xB0);
        out8(0x42, count as u8);
        out8(0x42, (count >> 8) as u8);
        let start = counter();
        // The channel's output goes high when the count reaches zero. A
        // missing PIT never sets it, so the wait is bounded.
        for _ in 0..100_000_000u32 {
            if in8(0x61) & 0x20 != 0 {
                return Some(measured_hz(counter() - start, count, PIT_HZ));
            }
        }
    }
    None
}

fn use_pm_timer(pm: PmTimer) {
    match pm.address {
        Address::Io(port) => {
            let port = u64::from(port);
            crate::reserved::reserve(KIND_IOPORT, port, port + 4);
            ADDRESS.store(port, Ordering::Relaxed);
        }
        Address::Memory(phys) => {
            crate::reserved::reserve_mmio(phys, 4);
            ADDRESS.store(crate::space::map_device(phys, 4), Ordering::Relaxed);
            PM_IN_MEMORY.store(1, Ordering::Relaxed);
        }
    }
    WIDTH.store(if pm.wide { 32 } else { 24 }, Ordering::Relaxed);
}

/// Finds the TSC and, when needed, the local APIC timer frequencies,
/// picks the clock source and starts the clock. Must run while the memory
/// map is still in place, after the local APIC is set up.
pub fn init(info: &BootInfo) {
    let mem = DirectMem(info);
    let tables = Tables::new(&mem, info.firmware_table_phys);
    let find = |signature| match &tables {
        Ok(tables) => tables.find(signature),
        Err(e) => Err(*e),
    };
    let hpet = match find(*b"HPET") {
        Ok(table) => table.and_then(acpi::hpet_base),
        Err(e) => {
            log!(Warning, "{e}");
            None
        }
    };
    let pm = match find(*b"FACP") {
        Ok(table) => table.and_then(acpi::pm_timer),
        Err(e) => {
            log!(Warning, "{e}");
            None
        }
    };
    let hpet = hpet.map(|base| {
        crate::reserved::reserve_mmio(base, HPET_SIZE);
        let virt = crate::space::map_device(base, HPET_SIZE);
        let config = hpet_read(virt, HPET_CONFIG);
        // SAFETY: setting the enable bit only starts the main counter.
        unsafe { ((virt + HPET_CONFIG) as *mut u64).write_volatile(config | HPET_ENABLE) };
        let capabilities = hpet_read(virt, HPET_CAPABILITIES);
        (virt, capabilities >> 32, capabilities & HPET_64BIT != 0)
    });

    if let Some(pm) = pm {
        use_pm_timer(pm);
    }
    // The references, in order of preference.
    let calibrate = |what: &str, counter: &dyn Fn() -> u64| -> u64 {
        hpet.map(|(virt, period_fs, wide)| {
            let hz = 1_000_000_000_000_000 / period_fs.max(1);
            // A 32-bit counter may wrap while it is measured.
            let ext = Extender::new(if wide { 64 } else { 32 });
            measure(counter, || ext.extend(hpet_read(virt, HPET_COUNTER)), hz)
        })
        .or_else(|| {
            pm.map(|_| {
                let ext = extender();
                measure(counter, || ext.extend(pm_read()), PM_TIMER_HZ)
            })
        })
        .or_else(|| measure_with_pit(counter))
        .unwrap_or_else(|| panic!("the {what} frequency could not be measured"))
    };

    let leaves = TscLeaves::read(|leaf| {
        let r = core::arch::x86_64::__cpuid(leaf);
        [r.eax, r.ebx, r.ecx, r.edx]
    });
    let tsc_hz = leaves
        .reported_hz()
        .unwrap_or_else(|| calibrate("TSC", &rdtsc));
    TSC_HZ.store(tsc_hz, Ordering::Relaxed);
    if !crate::timer::tsc_deadline() {
        crate::timer::start_count();
        let apic_hz = calibrate("local APIC timer", &crate::timer::count);
        crate::timer::set_hz(apic_hz);
    }

    let name;
    if leaves.invariant() {
        // The clock page gives user space the same factor and shift, so
        // that it computes exactly the values `now` returns.
        let (mult, shift) = tsc_scale(tsc_hz);
        RATE.store(u64::from(shift) << 32 | u64::from(mult), Ordering::Relaxed);
        SOURCE.store(TSC, Ordering::Relaxed);
        name = "the TSC";
    } else if let Some((virt, period_fs, wide)) = hpet {
        ADDRESS.store(virt, Ordering::Relaxed);
        RATE.store(period_fs, Ordering::Relaxed);
        WIDTH.store(if wide { 64 } else { 32 }, Ordering::Relaxed);
        SOURCE.store(HPET, Ordering::Relaxed);
        name = "the HPET";
    } else if pm.is_some() {
        RATE.store(PM_TIMER_HZ, Ordering::Relaxed);
        SOURCE.store(PM, Ordering::Relaxed);
        name = "the ACPI PM timer";
    } else {
        panic!("no clock source: no invariant TSC, HPET or ACPI PM timer");
    }
    BASE.store(counter(), Ordering::Release);
    log!(Info, "monotonic clock from {name}");
}

/// Creates the clock page, charged to `budget`. Its contents never change
/// after boot, so `sequence` stays 0.
pub fn create_clock_page(budget: &Arc<Budget>) -> Result<Arc<MemoryObject>, Error> {
    let tsc = (SOURCE.load(Ordering::Acquire) == TSC).then(|| {
        let rate = RATE.load(Ordering::Relaxed);
        let base = BASE.load(Ordering::Relaxed);
        (rate as u32, (rate >> 32) as u32, base, 0)
    });
    let header = clock_page(tsc);
    let page = MemoryObject::new(budget, PAGE_SIZE, 0)?;
    // This runs during boot, where no other thread could run.
    let mut work = crate::publish::Work::new(|| {});
    page.write(0, header.len() as u64, &mut work, |chunk| {
        chunk.copy_from_slice(&header[..chunk.len()]);
        Ok(())
    })?;
    Ok(page)
}
