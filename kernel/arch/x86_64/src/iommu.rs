//! The IOMMUs: finding them in the ACPI tables, enabling DMA translation
//! at boot, and letting devices reach pinned pages.

mod vtd;

use crate::log::log;
use crate::memory::DirectMem;
use crate::space;
use alloc::vec::Vec;
use boot_protocol::BootInfo;
use kernel_core::acpi::dmar::{self, Structure, Target};
use kernel_core::acpi::{Ecam, Tables};
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

/// The units that translate DMA, with the devices each covers.
struct Iommu {
    units: Vec<vtd::Unit>,
    covers: Vec<dmar::Unit>,
}

/// The IOMMUs when DMA is isolated, or `None` when it is not.
static IOMMU: SpinLock<Option<Iommu>> = SpinLock::new(None);

/// Reads configuration space through the MCFG table's ranges.
struct Config(Vec<Ecam>);

impl dmar::Config for Config {
    fn read8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u8 {
        let Some(phys) = self
            .0
            .iter()
            .filter(|e| e.segment == segment)
            .find_map(|e| e.address(bus, device, function, offset))
        else {
            return 0xFF;
        };
        let virt = space::map_device(phys, 1);
        // SAFETY: the byte is mapped uncached, and reading configuration
        // space has no side effects.
        let value = unsafe { (virt as *const u8).read_volatile() };
        space::unmap_device(virt, phys, 1);
        value
    }
}

/// Finds the VT-d units in the DMAR table, sets them up with the memory
/// that firmware reserves for devices and enables translation on all of
/// them. Without a DMAR table, logs that DMA is not isolated. When a unit
/// cannot be enabled, panics with `dma=strict` on the command line, and
/// otherwise logs it and leaves DMA unrestricted.
pub fn init(info: &BootInfo) {
    let mem = DirectMem(info);
    let tables = Tables::new(&mem, info.firmware_table_phys).unwrap_or_else(|e| panic!("{e}"));
    let found = |signature| tables.find(signature).unwrap_or_else(|e| panic!("{e}"));
    let Some(dmar) = found(*b"DMAR") else {
        log!(Warning, "no IOMMU: DMA is not isolated");
        return;
    };
    let config =
        Config(found(*b"MCFG").map_or(Vec::new(), |m| kernel_core::acpi::ecams(m).collect()));
    match enable(dmar, &config) {
        Ok(iommu) => {
            log!(Info, "DMA isolated by {} VT-d units", iommu.units.len());
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
fn enable(dmar: &[u8], config: &Config) -> Result<Iommu, Error> {
    let structures = dmar::structures(dmar);
    let resolve = |segment, scopes: &[dmar::Scope<'_>]| -> Vec<Target> {
        scopes
            .iter()
            .filter_map(|s| s.resolve(segment, config))
            .collect()
    };
    let mut iommu = Iommu {
        units: Vec::new(),
        covers: Vec::new(),
    };
    for s in &structures {
        if let Structure::Unit {
            segment,
            base,
            include_all,
            scopes,
        } = s
        {
            iommu.covers.push(dmar::Unit {
                segment: *segment,
                base: *base,
                include_all: *include_all,
                targets: resolve(*segment, scopes),
            });
            match vtd::Unit::new(*base) {
                Ok(unit) => iommu.units.push(unit),
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
                        iommu.reserve(rid, *base, *limit)?;
                    }
                }
            }
        }
        iommu.units.iter().try_for_each(vtd::Unit::enable)
    })();
    if let Err(e) = result {
        iommu.disable();
        return Err(e);
    }
    Ok(iommu)
}

impl Iommu {
    /// The unit that covers the device `rid`.
    fn unit(&mut self, rid: u64) -> Option<&mut vtd::Unit> {
        dmar::covering(&self.covers, rid).map(|i| &mut self.units[i])
    }

    /// Gives the device `rid` lasting read and write access to the memory
    /// from `base` to `limit`, inclusive, which firmware reserved for it.
    fn reserve(&mut self, rid: u64, base: u64, limit: u64) -> Result<(), Error> {
        let Some(unit) = self.unit(rid) else {
            return Ok(());
        };
        let mut page = base / PAGE_SIZE * PAGE_SIZE;
        while page <= limit {
            unit.map(rid, page, true, true)?;
            page += PAGE_SIZE;
        }
        unit.sync(rid)
    }

    fn disable(&self) {
        for unit in &self.units {
            unit.disable();
        }
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

/// Lets the device `rid` read, write or both the page at `phys`, at that
/// address, until `unmap` takes the pin back. Does nothing when DMA is
/// not isolated.
pub fn map(rid: u64, phys: u64, read: bool, write: bool) -> Result<(), Error> {
    let mut iommu = IOMMU.lock();
    match iommu.as_mut().and_then(|i| i.unit(rid)) {
        Some(unit) => unit.map(rid, phys, read, write),
        None => Ok(()),
    }
}

/// Takes back a pin that `map` made.
pub fn unmap(rid: u64, phys: u64, read: bool, write: bool) {
    let mut iommu = IOMMU.lock();
    if let Some(unit) = iommu.as_mut().and_then(|i| i.unit(rid)) {
        unit.unmap(rid, phys, read, write);
    }
}

/// Makes the pins and unpins of the device `rid` take effect, and returns
/// once the IOMMU no longer uses translations that were taken back.
pub fn sync(rid: u64) -> Result<(), Error> {
    let mut iommu = IOMMU.lock();
    match iommu.as_mut().and_then(|i| i.unit(rid)) {
        Some(unit) => unit.sync(rid),
        None => Ok(()),
    }
}
