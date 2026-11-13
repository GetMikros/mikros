//! Device registers mapped from a BAR.

use crate::memory::kernel;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE_SIZE: u64 = 4096;

/// A range of device memory mapped into the driver with the
/// `UNCACHED_DEVICE` policy.
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
    ) -> Result<Mmio, Status> {
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

    /// The length of the range in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn address(&self, offset: u64, width: u64) -> u64 {
        assert!(offset + width <= self.len, "access beyond the registers");
        self.at + offset
    }

    pub fn read32(&self, offset: u64) -> u32 {
        // SAFETY: `address` checked that the bytes lie in the mapping.
        unsafe { core::ptr::read_volatile(self.address(offset, 4) as *const u32) }
    }

    pub fn write32(&self, offset: u64, value: u32) {
        // SAFETY: as for `read32`.
        unsafe { core::ptr::write_volatile(self.address(offset, 4) as *mut u32, value) }
    }

    /// Reads a 64-bit register as two 32-bit halves, low first.
    pub fn read64(&self, offset: u64) -> u64 {
        u64::from(self.read32(offset)) | u64::from(self.read32(offset + 4)) << 32
    }

    /// Writes a 64-bit register as two 32-bit halves, low first.
    pub fn write64(&self, offset: u64, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }
}
