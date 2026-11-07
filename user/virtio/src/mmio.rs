//! Configuration structures mapped from BARs.

use crate::{Error, kernel};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE_SIZE: u64 = 4096;

/// A range of device memory mapped into the driver, uncached as device
/// memory.
pub struct Mmio {
    at: u64,
    len: u64,
    _mem: OwnedHandle,
}

impl Mmio {
    /// Maps the `len` bytes at physical address `base` from `resource`
    /// into `process`.
    pub fn map(
        process: sys::Handle,
        resource: &OwnedHandle,
        base: u64,
        len: u64,
    ) -> Result<Mmio, Error> {
        let start = base & !(PAGE_SIZE - 1);
        let end = (base + len.max(1)).next_multiple_of(PAGE_SIZE);
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe {
            sys::mem_create_physical(
                resource.raw(),
                start,
                end - start,
                UNCACHED_DEVICE,
                &mut mem,
            )
        })?;
        // SAFETY: the kernel just created the handle for us.
        let mem = unsafe { OwnedHandle::from_raw(mem) };
        let mut mapped = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        kernel(unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                mem.raw(),
                0,
                end - start,
                &mut mapped,
            )
        })?;
        Ok(Mmio {
            at: mapped + (base - start),
            len,
            _mem: mem,
        })
    }

    fn address(&self, offset: u64, width: u64) -> u64 {
        assert!(offset + width <= self.len, "access beyond the structure");
        self.at + offset
    }

    pub fn read8(&self, offset: u64) -> u8 {
        // SAFETY: `address` checked that the byte lies in the mapping.
        unsafe { core::ptr::read_volatile(self.address(offset, 1) as *const u8) }
    }

    pub fn read16(&self, offset: u64) -> u16 {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::read_volatile(self.address(offset, 2) as *const u16) }
    }

    pub fn read32(&self, offset: u64) -> u32 {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::read_volatile(self.address(offset, 4) as *const u32) }
    }

    pub fn write8(&self, offset: u64, value: u8) {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::write_volatile(self.address(offset, 1) as *mut u8, value) }
    }

    pub fn write16(&self, offset: u64, value: u16) {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::write_volatile(self.address(offset, 2) as *mut u16, value) }
    }

    pub fn write32(&self, offset: u64, value: u32) {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::write_volatile(self.address(offset, 4) as *mut u32, value) }
    }

    /// Writes `value` as two 32-bit halves, low first, as virtio allows
    /// for its 64-bit fields.
    pub fn write64(&self, offset: u64, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}
