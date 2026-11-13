//! Contiguous memory that both the driver and the device reach, for
//! queues, command tables and other structures the device reads.

use crate::PAGE;
use crate::memory::{BELOW_4G, DEVICE_READ, DEVICE_WRITE, kernel};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;

/// Physically contiguous memory, mapped into the driver and pinned for
/// the device.
pub struct Contiguous {
    at: u64,
    physical: u64,
    size: u64,
    _mem: OwnedHandle,
    _pmt: OwnedHandle,
}

impl Contiguous {
    /// `size` bytes, rounded up to a page, of zeroed contiguous memory
    /// from the `DMA` resource `dma`, below 4 GiB when `below_4g` is
    /// true, mapped into `process` and pinned through `bti`.
    pub fn new(
        dma: &OwnedHandle,
        bti: &OwnedHandle,
        process: sys::Handle,
        size: u64,
        below_4g: bool,
    ) -> Result<Contiguous, Status> {
        let size = size.max(1).next_multiple_of(PAGE);
        let options = if below_4g { BELOW_4G } else { 0 };
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe { sys::mem_create_contiguous(dma.raw(), size, 12, options, &mut mem) })?;
        // SAFETY: the kernel just created the handle for us.
        let mem = unsafe { OwnedHandle::from_raw(mem) };
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        kernel(unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                mem.raw(),
                0,
                size,
                &mut at,
            )
        })?;
        let mut pmt = 0;
        // SAFETY: the out pointer is valid for the call.
        let pinned = kernel(unsafe {
            sys::bti_pin(
                bti.raw(),
                mem.raw(),
                0,
                size,
                DEVICE_READ | DEVICE_WRITE,
                &mut pmt,
            )
        });
        if let Err(status) = pinned {
            // SAFETY: the range was mapped above and nothing refers to it.
            unsafe { sys::vm_unmap(process, at, size) };
            return Err(status);
        }
        // SAFETY: the kernel just created the handle for us.
        let pmt = unsafe { OwnedHandle::from_raw(pmt) };
        let mut physical = 0;
        // SAFETY: the out pointer is valid for the call.
        kernel(unsafe { sys::mem_get_physical(mem.raw(), &mut physical) })?;
        let memory = Contiguous {
            at,
            physical,
            size,
            _mem: mem,
            _pmt: pmt,
        };
        memory.zero();
        Ok(memory)
    }

    /// The device's address of byte `offset`.
    pub fn physical(&self, offset: u64) -> u64 {
        assert!(offset < self.size, "offset beyond the memory");
        self.physical + offset
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    fn address(&self, offset: u64, width: u64) -> u64 {
        assert!(offset + width <= self.size, "access beyond the memory");
        self.at + offset
    }

    /// Sets every byte to 0.
    pub fn zero(&self) {
        for offset in (0..self.size).step_by(8) {
            // SAFETY: the mapping covers `size` bytes; the device may
            // access them too, which volatile accesses allow for.
            unsafe { core::ptr::write_volatile((self.at + offset) as *mut u64, 0) };
        }
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

    pub fn read64(&self, offset: u64) -> u64 {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::read_volatile(self.address(offset, 8) as *const u64) }
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

    pub fn write64(&self, offset: u64, value: u64) {
        // SAFETY: as for `read8`.
        unsafe { core::ptr::write_volatile(self.address(offset, 8) as *mut u64, value) }
    }

    /// Copies `out.len()` bytes from byte `offset`.
    pub fn read_bytes(&self, offset: u64, out: &mut [u8]) {
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = self.read8(offset + i as u64);
        }
    }
}
