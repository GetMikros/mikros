//! The `AcpiOs` functions of ACPICA's operating system layer that need
//! nothing of the hardware: memory, locks, mutexes and semaphores, time,
//! signals from AML, the table overrides, and output.

use crate::sync::{FOREVER, Lock, Locked, Semaphore, now};
use crate::{AE_BAD_PARAMETER, AE_LIMIT, AE_OK, AE_TIME, Status};
use alloc::boxed::Box;
use alloc::string::String;
use core::alloc::Layout;
use core::ffi::{CStr, c_char, c_void};
use mikros_rt::log::{self, Severity};
use mikros_rt::sys;

/// The bytes before each block of `AcpiOsAllocate` that hold its size,
/// as many as the block's alignment.
const HEADER: usize = 16;
/// The timeout that waits for ever.
const WAIT_FOREVER: u16 = 0xFFFF;
const NANOS_PER_MILLI: i64 = 1_000_000;
/// The `AcpiOsSignal` functions.
const SIGNAL_FATAL: u32 = 0;
const SIGNAL_BREAKPOINT: u32 = 1;
/// The longest line that the output keeps before it logs it.
const LINE_MAX: usize = 512;

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

/// The timer in units of 100 ns.
#[unsafe(no_mangle)]
extern "C" fn AcpiOsGetTimer() -> u64 {
    (now() / 100) as u64
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsSleep(milliseconds: u64) {
    let nanos = i64::try_from(milliseconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(NANOS_PER_MILLI);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_sleep(now().saturating_add(nanos)) };
}

/// Waits `microseconds` without giving up the processor.
#[unsafe(no_mangle)]
extern "C" fn AcpiOsStall(microseconds: u32) {
    let end = now() + i64::from(microseconds) * 1000;
    while now() < end {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsEnterSleep(_state: u8, _a: u32, _b: u32) -> Status {
    AE_OK
}

/// What a fatal AML operation passes to `AcpiOsSignal`.
#[repr(C)]
struct FatalInfo {
    kind: u32,
    code: u32,
    argument: u32,
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsSignal(function: u32, info: *mut c_void) -> Status {
    if info.is_null() {
        return AE_OK;
    }
    match function {
        SIGNAL_FATAL => {
            // SAFETY: ACPICA passes a fatal operation's information.
            let info = unsafe { &*(info as *const FatalInfo) };
            mikros_rt::log!(
                Error,
                "acpi: AML fatal: type {:#x}, code {:#x}, argument {:#x}",
                info.kind,
                info.code,
                info.argument
            );
        }
        SIGNAL_BREAKPOINT => {
            // SAFETY: ACPICA passes the breakpoint's message.
            let message = unsafe { CStr::from_ptr(info as *const c_char) };
            mikros_rt::log!(Info, "acpi: AML breakpoint: {}", message.to_string_lossy());
        }
        _ => {}
    }
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsPredefinedOverride(_name: *const c_void, new: *mut *mut c_char) -> Status {
    if new.is_null() {
        return AE_BAD_PARAMETER;
    }
    // SAFETY: ACPICA passes a pointer to write the override to.
    unsafe { *new = core::ptr::null_mut() };
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsTableOverride(_table: *mut c_void, new: *mut *mut c_void) -> Status {
    if new.is_null() {
        return AE_BAD_PARAMETER;
    }
    // SAFETY: ACPICA passes a pointer to write the override to.
    unsafe { *new = core::ptr::null_mut() };
    AE_OK
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsPhysicalTableOverride(
    _table: *mut c_void,
    address: *mut u64,
    length: *mut u32,
) -> Status {
    if address.is_null() || length.is_null() {
        return AE_BAD_PARAMETER;
    }
    // SAFETY: ACPICA passes pointers to write the override to.
    unsafe {
        *address = 0;
        *length = 0;
    }
    AE_OK
}

/// What ACPICA has written of the line it is writing.
static LINE: Locked<String> = Locked::new(String::new());

/// The severity of a line of ACPICA's output, from how it starts.
fn severity(line: &str) -> Severity {
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| line.starts_with(p));
    if starts(&["ACPI Error", "ACPI Exception", "Firmware Error"]) {
        Severity::Error
    } else if starts(&["ACPI Warning", "Firmware Warning"]) {
        Severity::Warning
    } else {
        Severity::Info
    }
}

/// Takes a piece of ACPICA's output from the glue's `AcpiOsVprintf`, and
/// logs each line once it ends, or once it is too long.
#[unsafe(no_mangle)]
extern "C" fn mikros_acpi_print(text: *const c_char) {
    if text.is_null() {
        return;
    }
    // SAFETY: the glue passes a string that ends with a 0.
    let text = unsafe { CStr::from_ptr(text) }.to_bytes();
    let mut line = LINE.lock();
    for piece in text.split_inclusive(|&b| b == b'\n') {
        let ends = piece.last() == Some(&b'\n');
        let piece = if ends {
            &piece[..piece.len() - 1]
        } else {
            piece
        };
        line.push_str(&String::from_utf8_lossy(piece));
        if ends || line.len() >= LINE_MAX {
            let text = line.trim_end();
            if !text.is_empty() {
                log::write(severity(text), format_args!("acpi: {text}"));
            }
            line.clear();
        }
    }
}
