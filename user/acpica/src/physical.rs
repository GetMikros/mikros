//! Physical memory for ACPICA: the firmware's tables and the registers
//! of its memory operation regions, through `MMIO` resources that the
//! device manager grants, shared with other drivers, and physical memory
//! objects mapped uncached. A mapping that covers a later request is used
//! for it too, so that tables mapped again and again cost one request.

use crate::sync::Locked;
use crate::{AE_BAD_PARAMETER, AE_NO_MEMORY, AE_OK, Status};
use alloc::vec::Vec;
use core::ffi::c_void;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const KIND_MMIO: u32 = 1;
const UNCACHED_DEVICE: u32 = 2;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE: u64 = 4096;

/// Pages of physical memory mapped for ACPICA.
struct Mapping {
    /// The first physical address, a page's, and the bytes mapped.
    start: u64,
    len: u64,
    /// Where they are mapped.
    at: u64,
    /// The `map` calls not yet given back.
    users: u32,
    _resource: OwnedHandle,
    _mem: OwnedHandle,
}

static MAPPINGS: Locked<Vec<Mapping>> = Locked::new(Vec::new());

/// Maps the `len` bytes at the physical address `physical`, and gives
/// their address: None when the device manager or the kernel refused.
pub fn map(physical: u64, len: u64) -> Option<u64> {
    let start = physical & !(PAGE - 1);
    let end = physical
        .checked_add(len.max(1))?
        .checked_next_multiple_of(PAGE)?;
    let mut mappings = MAPPINGS.lock();
    if let Some(m) = mappings
        .iter_mut()
        .find(|m| m.start <= start && end <= m.start + m.len)
    {
        m.users += 1;
        return Some(m.at + (physical - m.start));
    }
    let size = end - start;
    let resource = crate::host().request(KIND_MMIO, start, size, true)?;
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let created =
        unsafe { sys::mem_create_physical(resource.raw(), start, size, UNCACHED_DEVICE, &mut mem) };
    if created != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let mapped = unsafe {
        sys::vm_map(
            crate::process(),
            PERM_READ | PERM_WRITE,
            0,
            mem.raw(),
            0,
            size,
            &mut at,
        )
    };
    if mapped != mikros_rt::OK {
        return None;
    }
    mappings.push(Mapping {
        start,
        len: size,
        at,
        users: 1,
        _resource: resource,
        _mem: mem,
    });
    Some(at + (physical - start))
}

/// Gives back a `map` of the address `at`, unmapping its pages when
/// nothing else uses them.
pub fn unmap(at: u64) {
    let mut mappings = MAPPINGS.lock();
    let Some(i) = mappings
        .iter()
        .position(|m| (m.at..m.at + m.len).contains(&at))
    else {
        return;
    };
    mappings[i].users -= 1;
    if mappings[i].users == 0 {
        let mapping = mappings.swap_remove(i);
        // SAFETY: nothing of ACPICA uses the range any more.
        unsafe { sys::vm_unmap(crate::process(), mapping.at, mapping.len) };
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsMapMemory(physical: u64, len: u64) -> *mut c_void {
    map(physical, len).map_or(core::ptr::null_mut(), |at| at as *mut c_void)
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsUnmapMemory(at: *mut c_void, _len: u64) {
    unmap(at as u64);
}

/// Reads `width` bits at `at`, which is mapped and aligned to them.
///
/// # Safety
///
/// `at` must be mapped for `width` bits.
pub unsafe fn read(at: u64, width: u32) -> Option<u64> {
    // SAFETY: the caller maps the bits at `at`.
    unsafe {
        Some(match width {
            8 => u64::from(core::ptr::read_volatile(at as *const u8)),
            16 => u64::from(core::ptr::read_volatile(at as *const u16)),
            32 => u64::from(core::ptr::read_volatile(at as *const u32)),
            64 => core::ptr::read_volatile(at as *const u64),
            _ => return None,
        })
    }
}

/// Writes the low `width` bits of `value` at `at`: false for a width
/// other than 8, 16, 32 or 64.
///
/// # Safety
///
/// `at` must be mapped writable for `width` bits.
pub unsafe fn write(at: u64, value: u64, width: u32) -> bool {
    // SAFETY: the caller maps the bits at `at`.
    unsafe {
        match width {
            8 => core::ptr::write_volatile(at as *mut u8, value as u8),
            16 => core::ptr::write_volatile(at as *mut u16, value as u16),
            32 => core::ptr::write_volatile(at as *mut u32, value as u32),
            64 => core::ptr::write_volatile(at as *mut u64, value),
            _ => return false,
        }
    }
    true
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsReadMemory(physical: u64, value: *mut u64, width: u32) -> Status {
    if value.is_null() || !matches!(width, 8 | 16 | 32 | 64) {
        return AE_BAD_PARAMETER;
    }
    let Some(at) = map(physical, u64::from(width / 8)) else {
        return AE_NO_MEMORY;
    };
    // SAFETY: `map` mapped the bits; ACPICA passes a pointer to write to.
    unsafe { *value = read(at, width).unwrap_or(0) };
    unmap(at);
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsWriteMemory(physical: u64, value: u64, width: u32) -> Status {
    if !matches!(width, 8 | 16 | 32 | 64) {
        return AE_BAD_PARAMETER;
    }
    let Some(at) = map(physical, u64::from(width / 8)) else {
        return AE_NO_MEMORY;
    };
    // SAFETY: `map` mapped the bits writable.
    unsafe { write(at, value, width) };
    unmap(at);
    AE_OK
}
