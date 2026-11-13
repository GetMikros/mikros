//! Threads that the tests start in their own process, and what they
//! leave behind, suspending and resuming them, and the threads of a
//! process and their registers.

use crate::{Result, STATE_SUSPENDED, SUSPENDED, check, ensure, now, thread_state, u64_at, wait};
use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::sys::{self, Handle};

pub static THREAD_RAN: AtomicU64 = AtomicU64::new(0);

pub extern "sysv64" fn thread_entry(value: u64, _arg2: u64) -> ! {
    THREAD_RAN.store(value, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Set by a thread when it starts, for a test that waits for it.
pub static STARTED: AtomicU64 = AtomicU64::new(0);

/// Reads the general registers of `thread`.
pub fn general_registers(thread: Handle) -> (i64, [u8; 160]) {
    let mut regs = [0u8; 160];
    // SAFETY: the pointer and length describe `regs`.
    let status = unsafe { sys::thread_read_state(thread, 0, regs.as_mut_ptr(), 160) };
    (status, regs)
}

/// The loops the spinning thread has made.
pub static SPINS: AtomicU64 = AtomicU64::new(0);

pub extern "sysv64" fn spin(_: u64, _: u64) -> ! {
    loop {
        SPINS.fetch_add(1, Ordering::AcqRel);
    }
}

/// The `THREADS` record of `process`: up to 128 object IDs and their
/// count.
pub fn thread_list(process: Handle) -> Result<([u64; 128], usize)> {
    let mut record = [0u8; 1024];
    let mut len = 0;
    // SAFETY: the pointers and length describe `record` and `len`.
    check("listing threads", unsafe {
        sys::object_get_info(process, 7, record.as_mut_ptr(), 1024, &mut len)
    })?;
    let count = len as usize / 8;
    let mut koids = [0u64; 128];
    for (i, k) in koids.iter_mut().enumerate().take(count) {
        *k = u64_at(&record, 8 * i);
    }
    Ok((koids, count))
}

pub fn get_thread(process: Handle, koid: u64) -> (i64, Handle) {
    let mut thread = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::process_get_thread(process, koid, &mut thread) };
    (status, thread)
}

/// Set by `signal_sleeper` with what its sleep returned and when.
pub static SLEEP_RESULT: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Sleeps until `deadline`, records the status and the time, and exits.
pub extern "sysv64" fn signal_sleeper(deadline: u64, _: u64) -> ! {
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::thread_sleep(deadline as i64) };
    SLEEP_RESULT[0].store(status as u64, Ordering::Release);
    SLEEP_RESULT[1].store(now() as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

pub fn suspend(thread: Handle) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_suspend(thread) }
}

pub fn resume_thread(thread: Handle) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_resume(thread) }
}

/// Suspends `thread` and waits until it is suspended.
pub fn suspend_and_wait(thread: Handle) -> Result {
    check("suspending", suspend(thread))?;
    let (status, _) = wait(thread, SUSPENDED, now() + 1_000_000_000);
    check("becoming suspended", status)?;
    ensure("being SUSPENDED", thread_state(thread)? == STATE_SUSPENDED)
}

/// Ends a thread whose entry is written in assembly.
pub extern "sysv64" fn end_asm_thread() -> ! {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}
