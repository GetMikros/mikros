//! Walks ACPI tables in fuzzed physical memory and decodes the tables the
//! kernel reads: the FADT for the PM timer, the MADT, the HPET table, and
//! the DMAR, IVRS and MCFG tables for the IOMMUs. Configuration space,
//! which DMAR device scopes are followed through, is read from the input.

#![no_main]

use kernel_core::acpi::{self, PhysRead, Tables, dmar, ivrs, madt};
use libfuzzer_sys::fuzz_target;

/// Physical memory that holds the input from address 0.
struct Memory<'a>(&'a [u8]);

impl PhysRead for Memory<'_> {
    fn read(&self, phys: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(phys).ok()?;
        self.0.get(start..start.checked_add(len)?)
    }
}

/// Configuration space whose bytes come from the input.
struct Config<'a>(&'a [u8]);

impl dmar::Config for Config<'_> {
    fn read8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u8 {
        if self.0.is_empty() {
            return 0xFF;
        }
        let at = usize::from(segment)
            ^ usize::from(bus) << 8
            ^ usize::from(device) << 3
            ^ usize::from(function)
            ^ usize::from(offset) << 4;
        self.0[at % self.0.len()]
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
    if let Ok(Some(table)) = tables.find(*b"DMAR") {
        let config = Config(data);
        let _ = dmar::flags(table);
        let mut units = Vec::new();
        for structure in dmar::structures(table) {
            if let dmar::Structure::Unit {
                segment,
                base,
                include_all,
                scopes,
            } = structure
            {
                units.push(dmar::Unit {
                    segment,
                    base,
                    include_all,
                    targets: scopes
                        .iter()
                        .filter_map(|s| s.resolve(segment, &config))
                        .collect(),
                    aliases: scopes
                        .iter()
                        .filter_map(|s| Some((0, s.alias(segment, &config)?)))
                        .collect(),
                });
            }
        }
        for rid in [0, 0xF8, 0x1_0000, u64::from(u16::MAX)] {
            if let Some(i) = dmar::covering(&units, rid) {
                let _ = units[i].alias(rid);
            }
        }
    }
    if let Ok(Some(table)) = tables.find(*b"IVRS") {
        for unit in ivrs::units(table) {
            for device in [0, 0xA0, u16::MAX] {
                let _ = unit.covers(u64::from(device));
                let _ = unit.alias(device);
            }
        }
        let _ = ivrs::reserved(table);
    }
    if let Ok(Some(table)) = tables.find(*b"MCFG") {
        for ecam in acpi::ecams(table) {
            let _ = ecam.address(ecam.buses.0, 31, 7, 0xFFF);
            let _ = ecam.address(u8::MAX, 0, 0, 0);
        }
    }
});
