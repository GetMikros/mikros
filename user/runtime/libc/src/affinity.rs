//! The processor count and the affinity of threads (MIKSPEC-0033.R49):
//! musl's `sysconf`, `sched_getaffinity`, `sched_setaffinity` and the
//! `pthread` versions call these, which answer with 0 or a count, or
//! with a negated error number.
//!
//! The processor count is what `system_get_cpu_count` stores, whatever
//! the calling thread's affinity is. A thread is named by its handle,
//! which the thread table keeps for the main thread and for every thread
//! that `pthread_create` made until it is joined or detached.

use crate::errno::{self, EINVAL, ESRCH};
use crate::{process, thread};
use mikros_rt::sys::{self, Handle};

/// `BUFFER_TOO_SMALL`, which `thread_get_affinity` fails with.
const BUFFER_TOO_SMALL: i64 = -15;

/// `sysconf`'s processor count: the number of processors, or the
/// negated error number of the failed call.
#[unsafe(no_mangle)]
extern "C" fn __mikros_cpu_count() -> i64 {
    let mut count = 0u32;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::system_get_cpu_count(&mut count) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    i64::from(count)
}

/// The error number of a failed `thread_get_affinity` or
/// `thread_set_affinity`: `EINVAL` for an argument that is invalid and,
/// for reading, a buffer that is too small.
fn error(status: i64, reading: bool) -> i64 {
    if status == -10 || (reading && status == BUFFER_TOO_SMALL) {
        return EINVAL;
    }
    errno::from_status(status)
}

/// Reads or sets, as `set` says, the affinity of the thread with the
/// handle `handle`, through the `size` bytes at `mask`.
fn affinity(handle: Handle, set: bool, size: usize, mask: *mut u8) -> i64 {
    let status = if set {
        // SAFETY: musl passes a mask of `size` bytes.
        unsafe { sys::thread_set_affinity(handle, mask, size as u64) }
    } else {
        // SAFETY: musl passes a buffer of `size` bytes.
        unsafe { sys::thread_get_affinity(handle, mask, size as u64) }
    };
    match status {
        mikros_rt::OK => 0,
        status => -error(status, !set),
    }
}

/// `sched_getaffinity` and `sched_setaffinity` for the thread `pid`: the
/// calling thread for 0, and the main thread for the process ID.
#[unsafe(no_mangle)]
extern "C" fn __mikros_sched_affinity(pid: i32, set: i32, size: usize, mask: *mut u8) -> i64 {
    let handle = if pid == 0 {
        thread::current()
    } else if i64::from(pid) == process::getpid() {
        thread::first()
    } else {
        None
    };
    match handle {
        Some(handle) => affinity(handle, set != 0, size, mask),
        None => -ESRCH,
    }
}

/// `pthread_getaffinity_np` and `pthread_setaffinity_np` for the thread
/// `thread`, a `pthread_t`, which the process created with `pthread_create`
/// and has not joined, or which is the main thread.
#[unsafe(no_mangle)]
extern "C" fn __mikros_thread_affinity(thread: usize, set: i32, size: usize, mask: *mut u8) -> i64 {
    match thread::handle_of_pthread(thread) {
        Some(handle) => affinity(handle, set != 0, size, mask),
        None => -ESRCH,
    }
}
