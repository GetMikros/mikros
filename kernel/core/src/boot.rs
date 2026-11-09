//! Reading what the bootloader left in memory.

use crate::bootinfo;
use crate::log::log;
use boot_protocol::{BootInfo, MemoryMapEntry};

/// The memory map entries of `info`, read through the direct map.
pub fn memory_map(info: &BootInfo) -> impl Iterator<Item = MemoryMapEntry> + Clone {
    let size = bootinfo::memory_map_size(info.memory_map_count, info.memory_map_entry_size)
        .ok()
        .flatten()
        .unwrap_or_else(|| panic!("the memory map size is invalid"));
    let address = info.direct_map_base + info.memory_map_phys;
    // SAFETY: the bootloader places the memory map in BootloaderReclaimable
    // memory covered by the direct map, and nothing frees that memory
    // while the kernel reads it.
    let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, size) };
    bootinfo::memory_map(bytes, info.memory_map_entry_size).unwrap_or_else(|e| panic!("{e}"))
}

/// Runs threads on an application processor, once the architecture has
/// set it up and given it its data.
pub fn start_secondary() -> ! {
    crate::sched::run()
}

/// Starts the kernel's services and the initial process on the boot
/// processor, and runs threads from then on. The architecture calls this
/// once it runs on the kernel's page tables and stack, with interrupts
/// disabled.
pub fn start(mut info: BootInfo) -> ! {
    // SAFETY: this runs once, on the boot processor, on the kernel's
    // tables and stack, with interrupts disabled.
    unsafe { crate::arch::early_setup(&mut info) };
    crate::wait::init();
    crate::sync::set_preempt_hook(crate::sched::preempt_point);
    // SAFETY: once, on the boot processor, after `early_setup`.
    unsafe { crate::arch::platform_setup(&info) };
    crate::random::init(&info);
    crate::sched::start_maintenance();
    crate::init::start(&info);
    // SAFETY: once; the initial process has the memory map and the
    // command line in its startup message, and nothing reads them after.
    let free = unsafe { crate::arch::boot_memory_reclaim(&info) };
    log!(
        Info,
        "kernel page tables active, {} MiB of memory free",
        (free * 4096) >> 20
    );
    crate::sched::run()
}
