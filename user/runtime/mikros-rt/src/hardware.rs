//! Safe forms of the system calls that drivers make on resources,
//! interrupts, I/O ports, physical and pinned memory, mappings, bus
//! transaction initiators and pins, so that their own code needs no
//! `unsafe`. Each takes slices for the kernel's pointers and returns a
//! handle the kernel created as an `OwnedHandle`. `Mapping` and `IoPorts`
//! are the accessors of device registers and of memory a device shares:
//! every access is checked against what was mapped or enabled, and is
//! volatile but for `Mapping::copy_from`.

use crate::interfaces::__midl::OwnedHandle;
use crate::sys::{self, Handle, Status};
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;

fn status(s: Status) -> Result<(), Status> {
    match s {
        crate::OK => Ok(()),
        s => Err(s),
    }
}

/// A handle that a successful call just created, `raw`.
fn created(s: Status, raw: Handle) -> Result<OwnedHandle, Status> {
    status(s)?;
    // SAFETY: the kernel just created the handle, and nothing else holds
    // its number.
    Ok(unsafe { OwnedHandle::from_raw(raw) })
}

/// The information record of a resource: its kind, then its base at
/// `0x08` and its size at `0x10`.
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;

/// What a resource covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resource {
    pub kind: u32,
    pub base: u64,
    pub size: u64,
}

/// The kind, base and size of the resource `resource`.
pub fn resource(resource: Handle) -> Result<Resource, Status> {
    let mut record = [0u8; INFO_RESOURCE_SIZE];
    crate::objects::info(resource, INFO_RESOURCE, &mut record)?;
    let u64_at = |at: usize| {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&record[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    let mut kind = [0u8; 4];
    kind.copy_from_slice(&record[..4]);
    Ok(Resource {
        kind: u32::from_le_bytes(kind),
        base: u64_at(0x08),
        size: u64_at(0x10),
    })
}

/// A new interrupt object for `irq`, which the resource `resource`
/// covers, with `options`.
pub fn interrupt(resource: Handle, irq: u32, options: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::interrupt_create(resource, irq, options, &mut out) };
    created(s, out)
}

/// `count` new message-signaled interrupts for the device of the `Bti`
/// `bti`, which the resource `resource` allows, with `options`. Fills
/// the first `count` records of 16 bytes of `messages`, which must hold
/// them, with the message of each.
pub fn msi_interrupts(
    resource: Handle,
    bti: Handle,
    count: u32,
    options: u32,
    messages: &mut [u8],
) -> Result<Vec<OwnedHandle>, Status> {
    assert!(
        messages.len() / 16 >= count as usize,
        "{} bytes for {count} messages",
        messages.len()
    );
    let mut out = vec![0; count as usize];
    // SAFETY: the pointers describe `count` handles and at least `count`
    // messages.
    let s = unsafe {
        sys::interrupt_create_msi(
            resource,
            bti,
            count,
            options,
            out.as_mut_ptr(),
            messages.as_mut_ptr(),
        )
    };
    status(s)?;
    Ok(out
        .into_iter()
        // SAFETY: the kernel just created the handles, and nothing else
        // holds their numbers.
        .map(|raw| unsafe { OwnedHandle::from_raw(raw) })
        .collect())
}

/// Acknowledges the interrupt `interrupt`, so that it can come again.
pub fn interrupt_ack(interrupt: Handle) -> Result<(), Status> {
    // SAFETY: the call takes no pointers.
    status(unsafe { sys::interrupt_ack(interrupt) })
}

/// A memory object for the `size` bytes of physical memory at `base`,
/// which the resource `resource` covers, with the cache policy `cache`.
pub fn physical_memory(
    resource: Handle,
    base: u64,
    size: u64,
    cache: u32,
) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::mem_create_physical(resource, base, size, cache, &mut out) };
    created(s, out)
}

/// A new memory object of `size` bytes whose pages do not move, from the
/// resource `resource`, with `options`.
pub fn pinned_memory(resource: Handle, size: u64, options: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::mem_create_pinned(resource, size, options, &mut out) };
    created(s, out)
}

/// Fills `pages` with the physical addresses of the pages of `memory`
/// from page `first`, one for each.
pub fn physical_pages(memory: Handle, first: u64, pages: &mut [u64]) -> Result<(), Status> {
    // SAFETY: the pointer and count describe `pages`.
    status(unsafe {
        sys::mem_get_physical_pages(memory, first, pages.len() as u64, pages.as_mut_ptr())
    })
}

/// A new `Bti` for the device of the requester ID `requester_id`, which
/// the resource `resource` covers, with `options`.
pub fn bti(resource: Handle, requester_id: u64, options: u32) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::bti_create(resource, requester_id, options, &mut out) };
    created(s, out)
}

/// Releases the pages that pins of the `Bti` `bti` left in its
/// quarantine.
pub fn release_quarantine(bti: Handle) -> Result<(), Status> {
    // SAFETY: the call takes no pointers.
    status(unsafe { sys::bti_release_quarantine(bti) })
}

/// Pins the `size` bytes of `memory` from `offset` for the device of the
/// `Bti` `bti`, with `options`, and gives the pin's `Pmt`.
pub fn pin(
    bti: Handle,
    memory: Handle,
    offset: u64,
    size: u64,
    options: u32,
) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let s = unsafe { sys::bti_pin(bti, memory, offset, size, options, &mut out) };
    created(s, out)
}

/// Unpins the pin `pmt`, which closes its handle. Closing the handle
/// without this leaves the pages in the quarantine instead.
pub fn unpin(pmt: OwnedHandle) -> Result<(), Status> {
    // SAFETY: the call takes no pointers, and closes the handle, whose
    // ownership it takes.
    status(unsafe { sys::pmt_unpin(pmt.into_raw()) })
}

/// Memory of an object mapped into a process, unmapped when dropped. Its
/// accessors are volatile, for device registers and for memory that a
/// device or another process also writes, and panic outside the mapping.
pub struct Mapping {
    process: Handle,
    at: u64,
    len: u64,
}

impl Mapping {
    /// Maps the `len` bytes of the memory object `memory` from `offset`
    /// into `process`, with the permissions `options`, where the kernel
    /// chooses.
    pub fn new(
        process: Handle,
        options: u32,
        memory: Handle,
        offset: u64,
        len: u64,
    ) -> Result<Mapping, Status> {
        let mut at = 0;
        // SAFETY: the out pointer is valid for the call; the kernel picks
        // a free range, which nothing else of the process uses.
        status(unsafe { sys::vm_map(process, options, 0, memory, offset, len, &mut at) })?;
        Ok(Mapping { process, at, len })
    }

    /// The address of the mapping's first byte.
    pub fn address(&self) -> u64 {
        self.at
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The address of the `size` bytes at `offset`, which must lie in the
    /// mapping and be aligned to `size`.
    fn at(&self, offset: u64, size: u64) -> u64 {
        assert!(
            offset.checked_add(size).is_some_and(|end| end <= self.len)
                && offset.is_multiple_of(size),
            "access of {size} bytes at {offset:#x} of a mapping of {:#x}",
            self.len
        );
        self.at + offset
    }

    pub fn read8(&self, offset: u64) -> u8 {
        // SAFETY: `at` checked that the byte lies in the mapping.
        unsafe { core::ptr::read_volatile(self.at(offset, 1) as *const u8) }
    }

    pub fn read16(&self, offset: u64) -> u16 {
        // SAFETY: `at` checked the bytes and their alignment.
        unsafe { core::ptr::read_volatile(self.at(offset, 2) as *const u16) }
    }

    pub fn read32(&self, offset: u64) -> u32 {
        // SAFETY: `at` checked the bytes and their alignment.
        unsafe { core::ptr::read_volatile(self.at(offset, 4) as *const u32) }
    }

    pub fn read64(&self, offset: u64) -> u64 {
        // SAFETY: `at` checked the bytes and their alignment.
        unsafe { core::ptr::read_volatile(self.at(offset, 8) as *const u64) }
    }

    pub fn write8(&self, offset: u64, value: u8) {
        // SAFETY: `at` checked that the byte lies in the mapping.
        unsafe { core::ptr::write_volatile(self.at(offset, 1) as *mut u8, value) }
    }

    pub fn write16(&self, offset: u64, value: u16) {
        // SAFETY: `at` checked the bytes and their alignment.
        unsafe { core::ptr::write_volatile(self.at(offset, 2) as *mut u16, value) }
    }

    pub fn write32(&self, offset: u64, value: u32) {
        // SAFETY: `at` checked the bytes and their alignment.
        unsafe { core::ptr::write_volatile(self.at(offset, 4) as *mut u32, value) }
    }

    pub fn write64(&self, offset: u64, value: u64) {
        // SAFETY: `at` checked the bytes and their alignment.
        unsafe { core::ptr::write_volatile(self.at(offset, 8) as *mut u64, value) }
    }

    /// Fills `buf` from the bytes at `offset`, one volatile read each.
    pub fn read(&self, offset: u64, buf: &mut [u8]) {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read8(offset + i as u64);
        }
    }

    /// Writes `data` to the bytes at `offset`, one volatile write each.
    pub fn write(&self, offset: u64, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.write8(offset + i as u64, b);
        }
    }

    /// Copies the `len` bytes at `from` in `source` to `to` in the
    /// mapping, as `core::ptr::copy` does rather than with volatile
    /// accesses: for memory that nothing else writes meanwhile.
    pub fn copy_from(&self, to: u64, source: &Mapping, from: u64, len: u64) {
        let to = self.range(to, len);
        let from = source.range(from, len);
        // SAFETY: `range` checked that both ranges lie in their mappings;
        // `copy` allows them to overlap.
        unsafe { core::ptr::copy(from as *const u8, to as *mut u8, len as usize) };
    }

    /// The address of the `len` bytes at `offset`, which must lie in the
    /// mapping.
    fn range(&self, offset: u64, len: u64) -> u64 {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "access of {len} bytes at {offset:#x} of a mapping of {:#x}",
            self.len
        );
        self.at + offset
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the call takes no pointers; nothing but the mapping
        // reached the range, which it no longer does.
        unsafe { sys::vm_unmap(self.process, self.at, self.len) };
    }
}

/// I/O ports that a process may use: the `count` from `base`, which the
/// resource it keeps covers. Its accessors panic outside them.
pub struct IoPorts {
    base: u16,
    count: u16,
    _resource: OwnedHandle,
}

impl IoPorts {
    /// Lets `process`, which must be the caller's, use the `count` ports
    /// from `base`, which `resource` covers.
    pub fn enable(
        process: Handle,
        resource: OwnedHandle,
        base: u16,
        count: u16,
    ) -> Result<IoPorts, Status> {
        // SAFETY: the call takes no pointers.
        status(unsafe {
            sys::process_ioports_enable(process, resource.raw(), base.into(), count.into())
        })?;
        Ok(IoPorts {
            base,
            count,
            _resource: resource,
        })
    }

    /// Port `offset` of the ports, for an access of `size` bytes.
    fn port(&self, offset: u16, size: u16) -> u16 {
        assert!(
            offset
                .checked_add(size)
                .is_some_and(|end| end <= self.count),
            "access of {size} ports at {offset:#x} of {:#x}",
            self.count
        );
        self.base + offset
    }

    pub fn read8(&self, offset: u16) -> u8 {
        let value: u8;
        // SAFETY: the process may use the port, which only the hardware
        // reads.
        unsafe {
            asm!("in al, dx", out("al") value, in("dx") self.port(offset, 1),
                 options(nomem, nostack, preserves_flags));
        }
        value
    }

    pub fn read16(&self, offset: u16) -> u16 {
        let value: u16;
        // SAFETY: as for `read8`.
        unsafe {
            asm!("in ax, dx", out("ax") value, in("dx") self.port(offset, 2),
                 options(nomem, nostack, preserves_flags));
        }
        value
    }

    pub fn read32(&self, offset: u16) -> u32 {
        let value: u32;
        // SAFETY: as for `read8`.
        unsafe {
            asm!("in eax, dx", out("eax") value, in("dx") self.port(offset, 4),
                 options(nomem, nostack, preserves_flags));
        }
        value
    }

    pub fn write8(&self, offset: u16, value: u8) {
        // SAFETY: as for `read8`.
        unsafe {
            asm!("out dx, al", in("dx") self.port(offset, 1), in("al") value,
                 options(nomem, nostack, preserves_flags));
        }
    }

    pub fn write16(&self, offset: u16, value: u16) {
        // SAFETY: as for `read8`.
        unsafe {
            asm!("out dx, ax", in("dx") self.port(offset, 2), in("ax") value,
                 options(nomem, nostack, preserves_flags));
        }
    }

    pub fn write32(&self, offset: u16, value: u32) {
        // SAFETY: as for `read8`.
        unsafe {
            asm!("out dx, eax", in("dx") self.port(offset, 4), in("eax") value,
                 options(nomem, nostack, preserves_flags));
        }
    }
}
