//! Configuration space through the ECAM range of the host bridge,
//! mapped uncached as device memory.

use crate::pci::{Address, Config, ecam_offset};
use mikros_rt::hardware::Mapping;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The configuration space of one bus.
const BUS_SIZE: u64 = 1 << 20;

/// The ECAM range of buses `bus_start` to `bus_end`, mapped as `mapping`.
pub struct Ecam {
    mapping: Mapping,
    bus_start: u8,
    bus_end: u8,
    _mem: OwnedHandle,
}

impl Ecam {
    /// Maps the ECAM range of buses `bus_start` to `bus_end`, which
    /// starts at `base`, from `resource` into `process`.
    pub fn map(
        process: sys::Handle,
        resource: &OwnedHandle,
        base: u64,
        bus_start: u8,
        bus_end: u8,
    ) -> Result<Ecam, sys::Status> {
        let size = (u64::from(bus_end) - u64::from(bus_start) + 1) * BUS_SIZE;
        let mem =
            mikros_rt::hardware::physical_memory(resource.raw(), base, size, UNCACHED_DEVICE)?;
        let mapping = Mapping::new(process, PERM_READ | PERM_WRITE, mem.raw(), 0, size)?;
        Ok(Ecam {
            mapping,
            bus_start,
            bus_end,
            _mem: mem,
        })
    }

    pub fn bus_start(&self) -> u8 {
        self.bus_start
    }

    pub fn bus_end(&self) -> u8 {
        self.bus_end
    }

    /// Where `offset` of `at` lies in the mapping.
    fn offset(&self, at: Address, offset: u16) -> u64 {
        ecam_offset(self.bus_start, at, offset) as u64
    }
}

impl Config for &Ecam {
    fn read(&self, at: Address, offset: u16, width: u8) -> u32 {
        let offset = self.offset(at, offset);
        match width {
            1 => u32::from(self.mapping.read8(offset)),
            2 => u32::from(self.mapping.read16(offset)),
            _ => self.mapping.read32(offset),
        }
    }

    fn write(&mut self, at: Address, offset: u16, width: u8, value: u32) {
        let offset = self.offset(at, offset);
        match width {
            1 => self.mapping.write8(offset, value as u8),
            2 => self.mapping.write16(offset, value as u16),
            _ => self.mapping.write32(offset, value),
        }
    }
}
