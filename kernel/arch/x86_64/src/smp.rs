//! Finding the other processors.

use crate::log::log;
use alloc::vec::Vec;
use kernel_core::acpi::madt;
use kernel_core::smp::{self, MAX_CPUS};
use kernel_core::sync::SpinLock;

/// The APIC ID of processor `n` at index `n`, set by `find`.
pub static APIC_IDS: SpinLock<Vec<u32>> = SpinLock::new(Vec::new());

/// Numbers the enabled processors that the MADT `madt` lists, with
/// `boot`, the boot processor's APIC ID, as processor 0, and logs those
/// beyond the kernel's limit.
pub fn find(madt: &[u8], boot: u32) {
    let found = smp::number(boot, madt::enabled_processors(madt), MAX_CPUS)
        .unwrap_or_else(|| panic!("no memory for the processor list"));
    for id in &found.beyond {
        log!(
            Warning,
            "processor with APIC ID {id} not used: more than {MAX_CPUS} processors"
        );
    }
    log!(Info, "{} processors found", found.used.len());
    *APIC_IDS.lock() = found.used;
}
