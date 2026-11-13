//! Configuration space through the ECAM range of the host bridge,
//! mapped uncached as device memory.

use crate::pci::{Address, Config, ecam_offset};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
/// The configuration space of one bus.
const BUS_SIZE: u64 = 1 << 20;

/// The ECAM range of buses `bus_start` to `bus_end`, mapped at `at`.
pub struct Ecam {
    at: u64,
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
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::mem_create_physical(resource.raw(), base, size, UNCACHED_DEVICE, &mut mem)
        };
        if status != mikros_rt::OK {
            return Err(status);
        }
        // SAFETY: the kernel just created the handle for us.
        let mem = unsafe { OwnedHandle::from_raw(mem) };
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let status = unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                mem.raw(),
                0,
                size,
                &mut at,
            )
        };
        if status != mikros_rt::OK {
            return Err(status);
        }
        Ok(Ecam {
            at,
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

    /// The address of `offset` of `at` in the mapping.
    fn address(&self, at: Address, offset: u16) -> u64 {
        self.at + ecam_offset(self.bus_start, at, offset) as u64
    }
}

impl Config for &Ecam {
    fn read(&self, at: Address, offset: u16, width: u8) -> u32 {
        let address = self.address(at, offset);
        // SAFETY: `address` lies in the mapping, aligned to `width`, as
        // the callers check.
        unsafe {
            match width {
                1 => u32::from(core::ptr::read_volatile(address as *const u8)),
                2 => u32::from(core::ptr::read_volatile(address as *const u16)),
                _ => core::ptr::read_volatile(address as *const u32),
            }
        }
    }

    fn write(&mut self, at: Address, offset: u16, width: u8, value: u32) {
        let address = self.address(at, offset);
        // SAFETY: as for `read`.
        unsafe {
            match width {
                1 => core::ptr::write_volatile(address as *mut u8, value as u8),
                2 => core::ptr::write_volatile(address as *mut u16, value as u16),
                _ => core::ptr::write_volatile(address as *mut u32, value),
            }
        }
    }
}
