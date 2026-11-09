//! `futex`, on the futexes of the kernel. musl waits and wakes with it.
//!
//! There is no requeueing: `FUTEX_REQUEUE` and `FUTEX_CMP_REQUEUE` wake
//! the waiters they would move instead, which musl's waiters take as a
//! spurious wakeup. Futexes are keyed by memory object, so the private
//! flag changes nothing.

use crate::clock;
use crate::errno::{self, EAGAIN, EINVAL, ENOSYS};
use mikros_rt::sys;

const FUTEX_WAIT: i64 = 0;
const FUTEX_WAKE: i64 = 1;
const FUTEX_REQUEUE: i64 = 3;
const FUTEX_CMP_REQUEUE: i64 = 4;
const FUTEX_PRIVATE: i64 = 128;
const NANOSECONDS: i64 = 1_000_000_000;
const FOREVER: i64 = i64::MAX;

/// A relative timeout, as `struct timespec`.
#[repr(C)]
pub struct Timeout {
    sec: i64,
    nsec: i64,
}

/// The monotonic deadline `timeout` from now, or no deadline for null.
fn deadline(timeout: *const Timeout) -> Result<i64, i64> {
    if timeout.is_null() {
        return Ok(FOREVER);
    }
    // SAFETY: musl passes a `struct timespec` or null.
    let t = unsafe { &*timeout };
    if t.sec < 0 || !(0..NANOSECONDS).contains(&t.nsec) {
        return Err(EINVAL);
    }
    let relative = t.sec.saturating_mul(NANOSECONDS).saturating_add(t.nsec);
    Ok(clock::monotonic()?.saturating_add(relative))
}

/// Wakes up to `count` waiters on `addr`, where a count from `i32::MAX`
/// up wakes all, and returns how many it woke.
fn wake(addr: *const u32, count: i64) -> i64 {
    let count = match count {
        ..=0 => return 0,
        c if c >= i64::from(i32::MAX) => u32::MAX,
        c => c as u32,
    };
    let mut woken = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::futex_wake(addr, count, &mut woken) };
    errno::result(status, i64::from(woken))
}

pub fn futex(addr: *const u32, op: i64, val: i64, timeout: *const Timeout, val3: i64) -> i64 {
    match op & !FUTEX_PRIVATE {
        FUTEX_WAIT => {
            let deadline = match deadline(timeout) {
                Ok(deadline) => deadline,
                Err(e) => return -e,
            };
            // SAFETY: the kernel checks the address.
            errno::result(unsafe { sys::futex_wait(addr, val as u32, deadline) }, 0)
        }
        FUTEX_WAKE => wake(addr, val),
        // The requeue count comes in the place of the timeout.
        FUTEX_REQUEUE => wake(addr, val.saturating_add(timeout as i64)),
        FUTEX_CMP_REQUEUE => {
            // SAFETY: musl passes the address of a futex word.
            if unsafe { core::ptr::read_volatile(addr) } != val3 as u32 {
                return -EAGAIN;
            }
            wake(addr, val.saturating_add(timeout as i64))
        }
        _ => -ENOSYS,
    }
}
