//! Walks ACPI tables in fuzzed physical memory and decodes the tables the
//! kernel reads: the FADT for the PM timer, the MADT and the HPET table.

#![no_main]

use kernel_core::acpi::{self, PhysRead, Tables, madt};
use libfuzzer_sys::fuzz_target;

/// Physical memory that holds the input from address 0.
struct Memory<'a>(&'a [u8]);

impl PhysRead for Memory<'_> {
    fn read(&self, phys: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(phys).ok()?;
        self.0.get(start..start.checked_add(len)?)
    }
}

fuzz_target!(|data: &[u8]| {
    let mem = Memory(data);
    let Ok(tables) = Tables::new(&mem, 0) else {
        return;
    };
    if let Ok(Some(fadt)) = tables.find(*b"FACP") {
        let _ = acpi::pm_timer(fadt);
    }
    if let Ok(Some(hpet)) = tables.find(*b"HPET") {
        let _ = acpi::hpet_base(hpet);
    }
    if let Ok(Some(table)) = tables.find(*b"APIC") {
        let _ = madt::local_apic_address(table);
        for _ in madt::entries(table) {}
        for _ in madt::nmi_inputs(table, 0) {}
    }
});
