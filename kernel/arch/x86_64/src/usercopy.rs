//! Copying between kernel memory and user memory.
//!
//! The kernel reaches user memory only through `copy_in` and `copy_out`.
//! A fault inside the copy routine resumes at its recovery label, so a
//! bad user address makes the copy fail instead of stopping the kernel.

use crate::aspace::in_user_range;
use crate::control::CR4_SMAP;
use crate::idt::Frame;
use core::arch::global_asm;
use kernel_core::status::Error;

// Copies RDX bytes from RSI to RDI and returns 0 in RAX, or 1 after a
// fault. When ECX is not zero, SMAP is on and user accesses are allowed
// only between STAC and CLAC.
global_asm!(
    ".global user_copy",
    ".global user_copy_prepared",
    ".global user_copy_end",
    ".global user_copy_fault",
    "user_copy_prepared:",
    "mov r9d, 1",
    "jmp 5f",
    "user_copy:",
    "xor r9d, r9d",
    "5:",
    "mov r8d, ecx",
    "test r8d, r8d",
    "jz 2f",
    "stac",
    "2:",
    "mov rcx, rdx",
    "rep movsb",
    "xor eax, eax",
    "jmp 3f",
    "user_copy_end:",
    "user_copy_fault:",
    "mov eax, 1",
    "3:",
    "test r8d, r8d",
    "jz 4f",
    "clac",
    "4:",
    "ret",
);

unsafe extern "sysv64" {
    fn user_copy(dst: *mut u8, src: *const u8, len: usize, smap: u32) -> u64;
    fn user_copy_prepared(dst: *mut u8, src: *const u8, len: usize, smap: u32) -> u64;
    static user_copy_end: u8;
    static user_copy_fault: u8;
}

fn smap() -> u32 {
    u32::from(crate::cpu::read_cr4() & CR4_SMAP != 0)
}

/// Copies `len` bytes from `src` to `dst`, one of which is a user address
/// that has been checked.
fn copy(dst: *mut u8, src: *const u8, len: usize) -> Result<(), Error> {
    // SAFETY: the kernel side is a valid buffer of `len` bytes, and the
    // user side lies in the user address range, where a fault resumes at
    // the recovery label.
    match unsafe { user_copy(dst, src, len, smap()) } {
        0 => Ok(()),
        _ => Err(Error::InvalidArgs),
    }
}

/// Copies `dst.len()` bytes from the user address `src` into `dst`. Fails
/// with `InvalidArgs` when the range is not in the user address range or
/// a page of it cannot be read.
pub fn copy_in(dst: &mut [u8], src: u64) -> Result<(), Error> {
    if dst.is_empty() {
        return Ok(());
    }
    if !in_user_range(src, dst.len() as u64) {
        return Err(Error::InvalidArgs);
    }
    copy(dst.as_mut_ptr(), src as *const u8, dst.len())
}

/// Copies `src` to the user address `dst`. Fails with `InvalidArgs` when
/// the range is not in the user address range or a page of it cannot be
/// written; bytes before the failing page may have been written.
pub fn copy_out(dst: u64, src: &[u8]) -> Result<(), Error> {
    if src.is_empty() {
        return Ok(());
    }
    if !in_user_range(dst, src.len() as u64) {
        return Err(Error::InvalidArgs);
    }
    copy(dst as *mut u8, src.as_ptr(), src.len())
}

/// Publishes bytes to a destination with stable writable page-table entries.
///
/// # Safety
///
/// Every destination page must be present, user-accessible and writable in
/// the current address space, with its translation and backing held stable
/// until this returns. The caller must not switch address spaces.
// SAFETY: the caller guarantees stable writable user mappings in the
// active address space for the duration of this bounded copy.
pub unsafe fn copy_prepared(dst: u64, src: &[u8]) {
    // SAFETY: the caller holds the checked destination stable; src is a
    // kernel slice. The assembly enables SMAP access only during the copy.
    let failed = unsafe { user_copy_prepared(dst as *mut u8, src.as_ptr(), src.len(), smap()) };
    assert_eq!(failed, 0, "prepared output lost its writable backing");
}

/// Whether the exception occurred in a copy that may resolve demand faults.
pub fn in_copy(frame: &Frame) -> bool {
    in_routine(frame) && frame.r9 == 0
}

fn in_routine(frame: &Frame) -> bool {
    let start = user_copy as *const () as u64;
    let end = (&raw const user_copy_end) as u64;
    frame.cs & 3 == 0 && (start..end).contains(&frame.rip)
}

/// Makes a fault inside the copy routine resume at its recovery label.
/// Returns whether the fault was one.
pub fn recover(frame: &mut Frame) -> bool {
    if !in_routine(frame) {
        return false;
    }
    frame.rip = (&raw const user_copy_fault) as u64;
    true
}
