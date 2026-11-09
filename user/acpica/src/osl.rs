//! The `AcpiOs` functions of ACPICA's operating system layer that need
//! nothing of the hardware: memory, locks, mutexes and semaphores.

use crate::sync::{FOREVER, Lock, Semaphore, now};
use crate::{AE_BAD_PARAMETER, AE_LIMIT, AE_OK, AE_TIME, Status};
use alloc::boxed::Box;
use core::alloc::Layout;
use core::ffi::c_void;

/// The bytes before each block of `AcpiOsAllocate` that hold its size,
/// as many as the block's alignment.
const HEADER: usize = 16;
/// The timeout that waits for ever.
const WAIT_FOREVER: u16 = 0xFFFF;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// The deadline of a wait of `timeout` milliseconds.
fn deadline(timeout: u16) -> i64 {
    if timeout == WAIT_FOREVER {
        FOREVER
    } else {
        now().saturating_add(i64::from(timeout) * NANOS_PER_MILLI)
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsInitialize() -> Status {
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsTerminate() -> Status {
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsGetRootPointer() -> u64 {
    crate::host().root_pointer()
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsAllocate(size: u64) -> *mut c_void {
    let Some(total) = usize::try_from(size)
        .ok()
        .and_then(|s| s.checked_add(HEADER))
    else {
        return core::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, HEADER) else {
        return core::ptr::null_mut();
    };
    // SAFETY: the layout is not empty.
    let block = unsafe { alloc::alloc::alloc(layout) };
    if block.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the block holds the header, aligned for a `usize`.
    unsafe {
        (block as *mut usize).write(total);
        block.add(HEADER) as *mut c_void
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsFree(memory: *mut c_void) {
    if memory.is_null() {
        return;
    }
    // SAFETY: `AcpiOsAllocate` gave the pointer, past the header that
    // holds the block's size.
    unsafe {
        let block = (memory as *mut u8).sub(HEADER);
        let total = (block as *const usize).read();
        alloc::alloc::dealloc(block, Layout::from_size_align_unchecked(total, HEADER));
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsCreateLock(out: *mut *mut c_void) -> Status {
    if out.is_null() {
        return AE_BAD_PARAMETER;
    }
    // SAFETY: ACPICA passes a pointer to write the handle to.
    unsafe { *out = Box::into_raw(Box::new(Lock::new())) as *mut c_void };
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsDeleteLock(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: `AcpiOsCreateLock` or `AcpiOsCreateMutex` made the
        // handle, which ACPICA no longer uses.
        drop(unsafe { Box::from_raw(handle as *mut Lock) });
    }
}

/// The lock of a handle of `AcpiOsCreateLock` or `AcpiOsCreateMutex`.
fn lock<'a>(handle: *mut c_void) -> &'a Lock {
    // SAFETY: ACPICA passes handles that it made and has not deleted.
    unsafe { &*(handle as *const Lock) }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsAcquireLock(handle: *mut c_void) -> u64 {
    lock(handle).take(FOREVER);
    0
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsReleaseLock(handle: *mut c_void, _flags: u64) {
    lock(handle).give();
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsCreateMutex(out: *mut *mut c_void) -> Status {
    AcpiOsCreateLock(out)
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsDeleteMutex(handle: *mut c_void) {
    AcpiOsDeleteLock(handle);
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsAcquireMutex(handle: *mut c_void, timeout: u16) -> Status {
    if handle.is_null() {
        return AE_BAD_PARAMETER;
    }
    if lock(handle).take(deadline(timeout)) {
        AE_OK
    } else {
        AE_TIME
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsReleaseMutex(handle: *mut c_void) {
    lock(handle).give();
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsCreateSemaphore(max: u32, initial: u32, out: *mut *mut c_void) -> Status {
    if out.is_null() || initial > max {
        return AE_BAD_PARAMETER;
    }
    // SAFETY: ACPICA passes a pointer to write the handle to.
    unsafe { *out = Box::into_raw(Box::new(Semaphore::new(max, initial))) as *mut c_void };
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsDeleteSemaphore(handle: *mut c_void) -> Status {
    if handle.is_null() {
        return AE_BAD_PARAMETER;
    }
    // SAFETY: `AcpiOsCreateSemaphore` made the handle, which ACPICA no
    // longer uses.
    drop(unsafe { Box::from_raw(handle as *mut Semaphore) });
    AE_OK
}

/// The semaphore of a handle of `AcpiOsCreateSemaphore`.
fn semaphore<'a>(handle: *mut c_void) -> &'a Semaphore {
    // SAFETY: ACPICA passes handles that it made and has not deleted.
    unsafe { &*(handle as *const Semaphore) }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsWaitSemaphore(handle: *mut c_void, units: u32, timeout: u16) -> Status {
    if handle.is_null() {
        return AE_BAD_PARAMETER;
    }
    if semaphore(handle).take(units, deadline(timeout)) {
        AE_OK
    } else {
        AE_TIME
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsSignalSemaphore(handle: *mut c_void, units: u32) -> Status {
    if handle.is_null() {
        return AE_BAD_PARAMETER;
    }
    if semaphore(handle).give(units) {
        AE_OK
    } else {
        AE_LIMIT
    }
}
