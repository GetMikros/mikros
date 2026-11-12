//! The IOMMUs: finding them in the ACPI tables, enabling DMA translation
//! at boot, letting devices reach pinned pages, remapping interrupts and
//! reporting blocked DMA.

mod amdvi;
mod vtd;

use crate::apic::DEVICE_VECTORS;
use crate::log::log;
use crate::memory::DirectMem;
use crate::space;
use alloc::vec::Vec;
use boot_protocol::BootInfo;
use kernel_core::acpi::dmar::{self, Structure, Target};
use kernel_core::acpi::ivrs::{self, Devices};
use kernel_core::acpi::{Ecam, Tables};
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// How interrupts reach the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remapping {
    /// Unremapped, in compatibility format.
    Off,
    /// Through VT-d entries, which I/O APICs and devices name by index in
    /// the remappable format.
    Vtd,
    /// Through the AMD-Vi interrupt table of the sending device, which
    /// the vector of an ordinary message selects.
    Amd,
}

/// The VT-d units with the devices each covers.
struct Vtd {
    units: Vec<vtd::Unit>,
    covers: Vec<dmar::Unit>,
    /// Whether remapping entries hold 32-bit destination IDs.
    wide: bool,
}

/// The AMD-Vi units with the devices each covers.
struct Amd {
    units: Vec<amdvi::Unit>,
    covers: Vec<ivrs::Unit>,
}

enum Units {
    Vtd(Vtd),
    Amd(Amd),
}

struct Iommu {
    units: Units,
    remapping: Remapping,
}

/// The IOMMUs when DMA is isolated, or `None` when it is not.
static IOMMU: SpinLock<Option<Iommu>> = SpinLock::new(kernel_core::lock_order::IOMMU, None);

/// Configuration space through the MCFG table's ranges.
struct Config(Vec<Ecam>);

impl Config {
    /// The 32-bit register at `offset` of a function, mapped for `f`, or
    /// `None` when no range covers the function.
    fn with<T>(
        &self,
        segment: u16,
        rid: u16,
        offset: u16,
        f: impl FnOnce(*mut u32) -> T,
    ) -> Option<T> {
        let (bus, device, function) = ((rid >> 8) as u8, (rid >> 3 & 0x1F) as u8, (rid & 7) as u8);
        let phys = self
            .0
            .iter()
            .filter(|e| e.segment == segment)
            .find_map(|e| e.address(bus, device, function, offset & !3))?;
        let virt = space::map_device(phys, 4);
        let value = f(virt as *mut u32);
        space::unmap_device(virt, phys, 4);
        Some(value)
    }

    fn read32(&self, segment: u16, rid: u16, offset: u16) -> u32 {
        // SAFETY: the register is mapped uncached, and reading
        // configuration space has no side effects.
        self.with(segment, rid, offset, |r| unsafe { r.read_volatile() })
            .unwrap_or(u32::MAX)
    }

    fn write32(&self, segment: u16, rid: u16, offset: u16, value: u32) {
        // SAFETY: the register is mapped uncached; callers write only the
        // IOMMU's own MSI capability.
        self.with(segment, rid, offset, |r| unsafe { r.write_volatile(value) });
    }

    /// Points the MSI capability of a function at `vector` on the boot
    /// processor and enables it. Returns whether it has one.
    fn enable_msi(&self, segment: u16, rid: u16, vector: u8) -> bool {
        let mut at = (self.read32(segment, rid, 0x34) & 0xFC) as u16;
        while at != 0 && self.read32(segment, rid, at) & 0xFF != 0x05 {
            at = (self.read32(segment, rid, at) >> 8 & 0xFC) as u16;
        }
        if at == 0 {
            return false;
        }
        let control = self.read32(segment, rid, at) >> 16;
        let id = crate::apic::boot_id() & 0xFF;
        self.write32(segment, rid, at + 4, 0xFEE0_0000 | id << 12);
        let data = if control & 1 << 7 != 0 {
            self.write32(segment, rid, at + 8, 0);
            at + 12
        } else {
            at + 8
        };
        self.write32(segment, rid, data, u32::from(vector));
        let header = self.read32(segment, rid, at);
        self.write32(segment, rid, at, header | 1 << 16);
        true
    }
}

impl Config {
    /// The device IDs of the functions that segment `segment` has.
    fn functions(&self, segment: u16) -> Vec<u16> {
        let mut found = Vec::new();
        for ecam in self.0.iter().filter(|e| e.segment == segment) {
            for bus in ecam.buses.0..=ecam.buses.1 {
                for device in 0..32u16 {
                    let first = u16::from(bus) << 8 | device << 3;
                    if self.read32(segment, first, 0) & 0xFFFF == 0xFFFF {
                        continue;
                    }
                    // Functions other than 0 exist only on multi-function
                    // devices, which bit 7 of the header type marks.
                    let multi = self.read32(segment, first, 0x0C) >> 16 & 0x80 != 0;
                    for function in 0..if multi { 8 } else { 1 } {
                        let rid = first | function;
                        if self.read32(segment, rid, 0) & 0xFFFF != 0xFFFF {
                            found.push(rid);
                        }
                    }
                }
            }
        }
        found
    }
}

impl dmar::Config for Config {
    fn read8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u8 {
        let rid = u16::from(bus) << 8 | u16::from(device) << 3 | u16::from(function);
        (self.read32(segment, rid, offset) >> (8 * (offset & 3))) as u8
    }
}

/// Whether the processor is AMD's or Hygon's, whose IOMMUs the IVRS table
/// describes.
fn amd_processor() -> bool {
    let r = core::arch::x86_64::__cpuid(0);
    let vendor = [r.ebx, r.edx, r.ecx].map(u32::to_le_bytes).concat();
    vendor == *b"AuthenticAMD" || vendor == *b"HygonGenuine"
}

/// Finds the IOMMUs in the DMAR or IVRS table, the one that matches the
/// processor when both exist, sets them up with the memory that firmware
/// reserves for devices, enables translation and, where every unit can,
/// interrupt remapping. Without either table, logs that DMA is not
/// isolated. When a unit cannot be enabled, panics with `dma=strict` on
/// the command line, and otherwise logs it and leaves DMA unrestricted.
pub fn init(info: &BootInfo) {
    let mem = DirectMem(info);
    let tables = Tables::new(&mem, info.firmware_table_phys).unwrap_or_else(|e| panic!("{e}"));
    let found = |signature| tables.find(signature).unwrap_or_else(|e| panic!("{e}"));
    let config =
        Config(found(*b"MCFG").map_or(Vec::new(), |m| kernel_core::acpi::ecams(m).collect()));
    let result = match (found(*b"DMAR"), found(*b"IVRS")) {
        (Some(dmar), None) => enable_vtd(dmar, &config),
        (Some(_), Some(ivrs)) if amd_processor() => enable_amd(ivrs, &config),
        (Some(dmar), Some(_)) => enable_vtd(dmar, &config),
        (None, Some(ivrs)) => enable_amd(ivrs, &config),
        (None, None) => {
            log!(Warning, "no IOMMU: DMA is not isolated");
            return;
        }
    };
    match result {
        Ok(iommu) => {
            let (count, kind) = match &iommu.units {
                Units::Vtd(v) => (v.units.len(), "VT-d"),
                Units::Amd(a) => (a.units.len(), "AMD-Vi"),
            };
            let remapped = if iommu.remapping == Remapping::Off {
                "not "
            } else {
                ""
            };
            log!(
                Info,
                "DMA isolated by {count} {kind} units, interrupts {remapped}remapped"
            );
            *IOMMU.lock() = Some(iommu);
        }
        Err(e) => {
            let cmdline = space::CMDLINE.lock();
            if kernel_core::cmdline::has_word(&cmdline.0[..cmdline.1], b"dma=strict") {
                panic!("enabling the IOMMU failed: {e:?}");
            }
            log!(
                Error,
                "enabling the IOMMU failed ({e:?}): DMA is not isolated"
            );
        }
    }
}

/// Sets up and enables every unit of `dmar`. On failure, translation is
/// off on every unit that had it turned on.
fn enable_vtd(dmar: &[u8], config: &Config) -> Result<Iommu, Error> {
    let structures = dmar::structures(dmar);
    let resolve = |segment, scopes: &[dmar::Scope<'_>]| -> Vec<Target> {
        scopes
            .iter()
            .filter_map(|s| s.resolve(segment, config))
            .collect()
    };
    let mut iommu = Iommu {
        units: Units::Vtd(Vtd {
            units: Vec::new(),
            covers: Vec::new(),
            wide: false,
        }),
        remapping: Remapping::Off,
    };
    let Units::Vtd(v) = &mut iommu.units else {
        unreachable!()
    };
    for s in &structures {
        if let Structure::Unit {
            segment,
            base,
            include_all,
            scopes,
        } = s
        {
            let aliases = scopes
                .iter()
                .filter_map(|s| {
                    let Target::Function(rid) = s.resolve(*segment, config)? else {
                        return None;
                    };
                    Some((rid, s.alias(*segment, config)?))
                })
                .collect();
            v.covers.push(dmar::Unit {
                segment: *segment,
                base: *base,
                include_all: *include_all,
                targets: resolve(*segment, scopes),
                aliases,
            });
            match vtd::Unit::new(*base) {
                Ok(unit) => v.units.push(unit),
                Err(e) => {
                    iommu.disable();
                    return Err(e);
                }
            }
        }
    }
    let result = (|| {
        for s in &structures {
            if let Structure::Reserved {
                segment,
                base,
                limit,
                scopes,
            } = s
            {
                for target in resolve(*segment, scopes) {
                    if let Target::Function(rid) = target {
                        iommu.reserve(rid, *base, *limit + 1, true, true)?;
                    }
                }
            }
        }
        let Units::Vtd(v) = &mut iommu.units else {
            unreachable!()
        };
        v.units.iter().try_for_each(vtd::Unit::enable)?;
        // Interrupts are remapped only when every unit can, as each
        // remaps those of the devices it covers. Entries carry 32-bit
        // destination IDs when the local APIC is in x2APIC mode and every
        // unit supports them, and otherwise 8-bit ones, which reach a
        // boot processor whose ID fits.
        let wide = crate::apic::x2apic() && v.units.iter().all(vtd::Unit::wide_ids);
        let can = dmar::flags(dmar) & dmar::INTR_REMAP != 0
            && v.units.iter().all(vtd::Unit::can_remap)
            && (wide || crate::apic::boot_id() <= 0xFF);
        if can {
            for unit in &mut v.units {
                unit.enable_remapping(wide)?;
            }
            iommu.remapping = Remapping::Vtd;
            v.wide = wide;
        }
        for unit in &v.units {
            unit.report_faults(
                crate::apic::IOMMU_FAULT_VECTOR,
                crate::apic::boot_id(),
                v.wide,
            );
        }
        Ok(())
    })();
    if let Err(e) = result {
        iommu.disable();
        return Err(e);
    }
    Ok(iommu)
}

/// Sets up and enables every unit of `ivrs`. On failure, every unit is
/// off again.
fn enable_amd(ivrs: &[u8], config: &Config) -> Result<Iommu, Error> {
    let mut iommu = Iommu {
        units: Units::Amd(Amd {
            units: Vec::new(),
            covers: ivrs::units(ivrs),
        }),
        remapping: Remapping::Off,
    };
    let result = (|| {
        let Units::Amd(a) = &mut iommu.units else {
            unreachable!()
        };
        for cover in &a.covers {
            a.units.push(amdvi::Unit::new(cover)?);
        }
        // Memory reserved for every device is given to each function
        // that configuration space shows.
        let mut present: Option<Vec<u16>> = None;
        for r in ivrs::reserved(ivrs) {
            let devices: Vec<u16> = match r.devices {
                Devices::All => present
                    .get_or_insert_with(|| config.functions(r.segment))
                    .clone(),
                Devices::Range(range) => range.collect(),
            };
            for device in devices {
                let rid = u64::from(r.segment) << 16 | u64::from(device);
                iommu.reserve(rid, r.base, r.base + r.length, r.read, r.write)?;
            }
        }
        // The units' remapping tables hold 8-bit destination IDs.
        let Units::Amd(a) = &mut iommu.units else {
            unreachable!()
        };
        if crate::apic::boot_id() <= 0xFF {
            for unit in &mut a.units {
                unit.enable_remapping()?;
            }
            iommu.remapping = Remapping::Amd;
        }
        for cover in &a.covers {
            let vector = crate::apic::IOMMU_FAULT_VECTOR;
            if !config.enable_msi(cover.segment, cover.device, vector) {
                log!(Warning, "IOMMU {:#x}: no MSI for its event log", cover.base);
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        iommu.disable();
        return Err(e);
    }
    Ok(iommu)
}

/// A unit of either kind, with the device ID it knows the device by.
enum Unit<'a> {
    Vtd(&'a mut vtd::Unit, u64),
    Amd(&'a mut amdvi::Unit, u16),
}

impl Unit<'_> {
    fn map(&mut self, phys: u64, read: bool, write: bool) -> Result<(), Error> {
        match self {
            Unit::Vtd(u, rid) => u.map(*rid, phys, read, write),
            Unit::Amd(u, device) => u.map(*device, phys, read, write),
        }
    }

    fn unmap(&mut self, phys: u64, read: bool, write: bool) {
        match self {
            Unit::Vtd(u, rid) => u.unmap(*rid, phys, read, write),
            Unit::Amd(u, device) => u.unmap(*device, phys, read, write),
        }
    }

    fn sync(&mut self) -> Result<(), Error> {
        match self {
            Unit::Vtd(u, rid) => u.sync(*rid),
            Unit::Amd(u, device) => u.sync(*device),
        }
    }
}

impl Iommu {
    /// The unit that covers the device `rid`.
    fn unit(&mut self, rid: u64) -> Option<Unit<'_>> {
        match &mut self.units {
            Units::Vtd(v) => {
                let i = dmar::covering(&v.covers, rid)?;
                Some(Unit::Vtd(&mut v.units[i], rid))
            }
            Units::Amd(a) => {
                let i = a.covers.iter().position(|c| c.covers(rid))?;
                Some(Unit::Amd(a.units.get_mut(i)?, rid as u16))
            }
        }
    }

    /// The requester ID the DMA of `rid` reaches its unit with, when it
    /// is another one.
    fn alias(&self, rid: u64) -> Option<u64> {
        match &self.units {
            Units::Vtd(v) => v.covers[dmar::covering(&v.covers, rid)?].alias(rid),
            Units::Amd(a) => {
                let unit = a.covers.iter().find(|c| c.covers(rid))?;
                let alias = unit.alias(rid as u16)?;
                Some(rid & !0xFFFF | u64::from(alias))
            }
        }
    }

    /// `rid` and its alias, if it has one.
    fn ids(&self, rid: u64) -> [Option<u64>; 2] {
        [Some(rid), self.alias(rid)]
    }

    /// Gives the device `rid`, under its alias too, lasting access to the
    /// memory from `base` to `end`, exclusive, which firmware reserved for
    /// it.
    fn reserve(
        &mut self,
        rid: u64,
        base: u64,
        end: u64,
        read: bool,
        write: bool,
    ) -> Result<(), Error> {
        for id in self.ids(rid).into_iter().flatten() {
            let Some(mut unit) = self.unit(id) else {
                continue;
            };
            let mut page = base / PAGE_SIZE * PAGE_SIZE;
            while page < end {
                unit.map(page, read, write)?;
                page += PAGE_SIZE;
            }
            unit.sync()?;
        }
        Ok(())
    }

    fn disable(&self) {
        match &self.units {
            Units::Vtd(v) => v.units.iter().for_each(vtd::Unit::disable),
            Units::Amd(a) => a.units.iter().for_each(amdvi::Unit::disable),
        }
    }
}

/// In the diagnostic build, calls `each` with the device, domain ID and
/// pinned pages of every domain. Without `wait`, does nothing while the
/// IOMMUs are locked.
#[cfg(feature = "diag")]
pub fn domains(
    wait: bool,
    each: &mut dyn FnMut(u64, u16, usize) -> core::fmt::Result,
) -> core::fmt::Result {
    let iommu = match wait {
        true => Some(IOMMU.lock()),
        false => IOMMU.try_lock(),
    };
    let Some(Some(iommu)) = iommu.as_deref() else {
        return Ok(());
    };
    match &iommu.units {
        Units::Vtd(v) => v.units.iter().try_for_each(|u| u.domains(each)),
        Units::Amd(a) => a.units.iter().try_for_each(|u| u.domains(each)),
    }
}

/// Whether DMA is isolated and the device `rid` is behind none of the
/// units, so that pins for it cannot take effect.
pub fn unknown(rid: u64) -> bool {
    IOMMU
        .lock()
        .as_mut()
        .is_some_and(|iommu| iommu.unit(rid).is_none())
}

/// Lets the device `rid`, under its alias too, read, write or both the
/// page at `phys`, at that address, until `unmap` takes the pin back.
/// Does nothing when DMA is not isolated.
pub fn map(rid: u64, phys: u64, read: bool, write: bool) -> Result<(), Error> {
    let mut iommu = IOMMU.lock();
    let Some(iommu) = iommu.as_mut() else {
        return Ok(());
    };
    let [own, alias] = iommu.ids(rid);
    if let Some(mut unit) = own.and_then(|id| iommu.unit(id)) {
        unit.map(phys, read, write)?;
    }
    if let Some(mut unit) = alias.and_then(|id| iommu.unit(id))
        && let Err(e) = unit.map(phys, read, write)
    {
        if let Some(mut unit) = iommu.unit(rid) {
            unit.unmap(phys, read, write);
        }
        return Err(e);
    }
    Ok(())
}

/// Takes back a pin that `map` made.
pub fn unmap(rid: u64, phys: u64, read: bool, write: bool) {
    let mut iommu = IOMMU.lock();
    let Some(iommu) = iommu.as_mut() else {
        return;
    };
    for id in iommu.ids(rid).into_iter().flatten() {
        if let Some(mut unit) = iommu.unit(id) {
            unit.unmap(phys, read, write);
        }
    }
}

/// Makes the pins and unpins of the device `rid`, and of its alias, take
/// effect, and returns once the IOMMU no longer uses translations that
/// were taken back.
pub fn sync(rid: u64) -> Result<(), Error> {
    let mut iommu = IOMMU.lock();
    let Some(iommu) = iommu.as_mut() else {
        return Ok(());
    };
    for id in iommu.ids(rid).into_iter().flatten() {
        if let Some(mut unit) = iommu.unit(id) {
            unit.sync()?;
        }
    }
    Ok(())
}

/// How interrupts reach the processor.
pub fn remapping() -> Remapping {
    IOMMU
        .lock()
        .as_ref()
        .map_or(Remapping::Off, |i| i.remapping)
}

/// The requester ID that the interrupts of the I/O APIC with ID `id`
/// carry, from the DMAR or IVRS table.
pub fn io_apic_source(id: u8) -> Option<u64> {
    let iommu = IOMMU.lock();
    match &iommu.as_ref()?.units {
        Units::Vtd(v) => v.covers.iter().find_map(|u| {
            u.targets.iter().find_map(|t| match t {
                Target::IoApic(i, rid) if *i == id => Some(*rid),
                _ => None,
            })
        }),
        Units::Amd(a) => a.covers.iter().find_map(|u| {
            let device = u.io_apics.iter().find(|(i, _)| *i == id)?.1;
            Some(u64::from(u.segment) << 16 | u64::from(device))
        }),
    }
}

/// Makes the remapping entry of `vector` for interrupts from the requester
/// ID `source` deliver the vector to the boot processor with fixed
/// delivery, level triggered when `level` is `Some(true)`, or blocks it
/// when `level` is `None`. On VT-d the entry is the vector's index in
/// every unit's table and checks the source; on AMD-Vi it is the vector's
/// entry in the table of the source's unit for that device. Does nothing
/// unless interrupts are remapped.
pub fn remap(vector: u8, source: u64, level: Option<bool>) -> Result<(), Error> {
    let mut guard = IOMMU.lock();
    let Some(iommu) = guard.as_mut() else {
        return Ok(());
    };
    if iommu.remapping == Remapping::Off {
        return Ok(());
    }
    let id = crate::apic::boot_id();
    match &mut iommu.units {
        Units::Vtd(v) => {
            let entry = match level {
                Some(level) => {
                    let id = u64::from(id);
                    let destination = if v.wide { id } else { id << 8 };
                    let low =
                        1 | u64::from(level) << 4 | u64::from(vector) << 16 | destination << 32;
                    // Verify the full 16-bit source ID.
                    [low, source & 0xFFFF | 1 << 18]
                }
                None => [0, 0],
            };
            let index = u16::from(vector - DEVICE_VECTORS.start);
            v.units
                .iter_mut()
                .try_for_each(|u| u.set_remap(index, entry))
        }
        Units::Amd(a) => {
            let Some(i) = a.covers.iter().position(|c| c.covers(source)) else {
                return Err(Error::NotSupported);
            };
            // Remapping on, fixed delivery in physical mode.
            let entry = match level {
                Some(_) => 1 | (id & 0xFF) << 8 | u32::from(vector) << 16,
                None => 0,
            };
            a.units[i].set_remap(source as u16, vector, entry)
        }
    }
}

/// How often blocked DMA of one device is logged at most, in
/// nanoseconds.
const FAULT_LOG_INTERVAL: i64 = 1_000_000_000;

/// The devices whose blocked DMA was logged last, with when. The oldest
/// entry makes room for a new device.
static LOGGED: SpinLock<[(u64, i64); 16]> = SpinLock::new(
    kernel_core::lock_order::IOMMU_LOGGED,
    [(u64::MAX, i64::MIN); 16],
);

/// Logs blocked DMA from `source`, unless it was logged less than a
/// second before `now`.
fn log_fault(source: u64, addr: u64, write: bool, now: i64) {
    let mut logged = LOGGED.lock();
    let slot = match logged.iter().position(|&(rid, _)| rid == source) {
        Some(i) => i,
        None => {
            let oldest = logged.iter().enumerate().min_by_key(|(_, e)| e.1);
            oldest.map_or(0, |(i, _)| i)
        }
    };
    let (rid, last) = logged[slot];
    if rid == source && now.saturating_sub(last) < FAULT_LOG_INTERVAL {
        return;
    }
    logged[slot] = (source, now);
    drop(logged);
    let direction = if write { "write" } else { "read" };
    log!(
        Warning,
        "blocked DMA {direction} from requester {source:#x} at {addr:#x}"
    );
}

/// Handles a fault event: logs each fault the units recorded, at most
/// once per second for each requester ID, and clears it.
pub fn fault_interrupt() {
    crate::apic::eoi();
    let iommu = IOMMU.lock();
    let Some(iommu) = iommu.as_ref() else {
        return;
    };
    let now = crate::time::now();
    match &iommu.units {
        Units::Vtd(v) => {
            for (unit, covers) in v.units.iter().zip(&v.covers) {
                let segment = u64::from(covers.segment) << 16;
                unit.take_faults(|sid, addr, write| log_fault(segment | sid, addr, write, now));
            }
        }
        Units::Amd(a) => {
            for (unit, covers) in a.units.iter().zip(&a.covers) {
                let segment = u64::from(covers.segment) << 16;
                unit.take_faults(|device, addr, write| {
                    log_fault(segment | u64::from(device), addr, write, now);
                });
            }
        }
    }
}
