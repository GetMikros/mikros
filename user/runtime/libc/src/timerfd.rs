//! timerfd: descriptors whose reads count the expirations of a timer on
//! the monotonic clock. Each has a kernel `Timer`, which is signaled once
//! its deadline passes; the C library keeps the period, works out how
//! many periods have passed when the descriptor is read, and sets the
//! timer to the next expiration. A timer descriptor stays in its process.

use crate::clock::{self, CLOCK_MONOTONIC};
use crate::errno::{self, EAGAIN, EFAULT, EINVAL};
use crate::fd::{self, Description};
use crate::fs::{O_CLOEXEC, O_NONBLOCK, O_RDWR};
use crate::lock::Lock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

/// The signal of a timer whose deadline has passed.
pub const SIGNALED: u32 = 1 << 3;
pub const TFD_NONBLOCK: i64 = O_NONBLOCK as i64;
pub const TFD_CLOEXEC: i64 = O_CLOEXEC as i64;
/// `timerfd_settime`'s flag for a deadline on the clock, not after now.
pub const TFD_TIMER_ABSTIME: i64 = 1;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
const FOREVER: i64 = i64::MAX;

/// `struct itimerspec`: the period, then the time of the next expiration.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Itimerspec {
    interval_sec: i64,
    interval_nsec: i64,
    value_sec: i64,
    value_nsec: i64,
}

/// The nanoseconds of a `timespec`, or `EINVAL` for one out of range.
fn nanos(sec: i64, nsec: i64) -> Result<i64, i64> {
    if sec < 0 || !(0..NANOS_PER_SECOND).contains(&nsec) {
        return Err(EINVAL);
    }
    sec.checked_mul(NANOS_PER_SECOND)
        .and_then(|ns| ns.checked_add(nsec))
        .ok_or(EINVAL)
}

/// The arming of a timer: when it next expires on the monotonic clock,
/// if it is armed, and its period in nanoseconds, zero for a timer that
/// expires once.
#[derive(Clone, Copy, Default)]
pub struct Arming {
    pub next: Option<i64>,
    pub interval: i64,
}

impl Arming {
    /// The expirations up to `now`, and the arming that follows them: a
    /// periodic timer's next expiration after `now`, and none for a timer
    /// that expires once and did.
    pub fn expire(self, now: i64) -> (u64, Arming) {
        match self.next {
            Some(next) if now >= next => {
                if self.interval == 0 {
                    return (1, Arming::default());
                }
                let periods = (now - next) / self.interval + 1;
                let following = next.saturating_add(periods.saturating_mul(self.interval));
                let after = Arming {
                    next: Some(following),
                    ..self
                };
                (periods as u64, after)
            }
            _ => (0, self),
        }
    }

    /// `struct itimerspec` for this arming at `now`: the time left until
    /// the next expiration, which for a periodic timer that has expired
    /// unread is the next one after `now`, and the period.
    pub fn spec(self, now: i64) -> Itimerspec {
        let left = match self.expire(now) {
            (
                _,
                Arming {
                    next: Some(next), ..
                },
            ) => next - now,
            _ => 0,
        };
        Itimerspec {
            interval_sec: self.interval / NANOS_PER_SECOND,
            interval_nsec: self.interval % NANOS_PER_SECOND,
            value_sec: left / NANOS_PER_SECOND,
            value_nsec: left % NANOS_PER_SECOND,
        }
    }

    /// The arming that `spec` asks for at `now`, with its first expiration
    /// at the time it gives when `absolute`, and after `now` otherwise. A
    /// zero time disarms the timer.
    pub fn from_spec(spec: &Itimerspec, absolute: bool, now: i64) -> Result<Arming, i64> {
        let interval = nanos(spec.interval_sec, spec.interval_nsec)?;
        let value = nanos(spec.value_sec, spec.value_nsec)?;
        if value == 0 {
            return Ok(Arming::default());
        }
        let next = if absolute {
            value
        } else {
            now.saturating_add(value)
        };
        Ok(Arming {
            next: Some(next),
            interval,
        })
    }
}

/// A timer descriptor's open description.
pub struct Timerfd {
    pub timer: OwnedHandle,
    arming: Lock<Arming>,
    /// The `O_` status flags, `O_RDWR` and `O_NONBLOCK` when reads do not
    /// wait, which `fcntl` changes.
    pub flags: AtomicU32,
}

impl Timerfd {
    /// Sets the kernel timer for `arming`: to its next expiration, or
    /// cancelled when it has none. Either clears the timer's signal.
    fn set(&self, arming: Arming) -> Result<(), i64> {
        // SAFETY: the calls take no pointers.
        let status = match arming.next {
            Some(next) => unsafe { sys::timer_set(self.timer.raw(), next) },
            None => unsafe { sys::timer_cancel(self.timer.raw()) },
        };
        if status == mikros_rt::OK {
            Ok(())
        } else {
            Err(errno::from_status(status))
        }
    }
}

/// The timer behind descriptor `fd`.
fn timer(fd: i64) -> Result<Arc<Description>, i64> {
    let description = fd::get(fd).ok_or(crate::errno::EBADF)?;
    match &*description {
        Description::Timer(_) => Ok(description),
        _ => Err(EINVAL),
    }
}

fn result(r: Result<i64, i64>) -> i64 {
    r.unwrap_or_else(|e| -e)
}

/// `timerfd_create`: a disarmed timer on `CLOCK_MONOTONIC`, the only
/// clock it takes, whose reads fail with `EAGAIN` instead of waiting with
/// `TFD_NONBLOCK`, as with `O_NONBLOCK` set later, and which closes on
/// `exec` with `TFD_CLOEXEC`.
pub fn create(clock: i64, flags: i64) -> i64 {
    if clock != CLOCK_MONOTONIC || flags & !(TFD_NONBLOCK | TFD_CLOEXEC) != 0 {
        return -EINVAL;
    }
    let mut timer = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::timer_create(0, &mut timer) };
    if status != mikros_rt::OK {
        return -errno::from_status(status);
    }
    let timerfd = Timerfd {
        // SAFETY: the kernel just created the handle for us.
        timer: unsafe { OwnedHandle::from_raw(timer) },
        arming: Lock::new(Arming::default()),
        flags: AtomicU32::new(match flags & TFD_NONBLOCK != 0 {
            true => O_RDWR | O_NONBLOCK,
            false => O_RDWR,
        }),
    };
    fd::insert(
        Arc::new(Description::Timer(timerfd)),
        flags & TFD_CLOEXEC != 0,
        0,
    )
}

/// `timerfd_settime`: arms or disarms the timer of `fd` as `new` asks,
/// relative to now unless `flags` has `TFD_TIMER_ABSTIME`, after storing
/// its arming before the call in `old` unless that is null. Expirations
/// not read yet are dropped.
pub fn settime(fd: i64, flags: i64, new: *const Itimerspec, old: *mut Itimerspec) -> i64 {
    result((|| {
        if flags & !TFD_TIMER_ABSTIME != 0 {
            return Err(EINVAL);
        }
        if new.is_null() {
            return Err(EFAULT);
        }
        let description = timer(fd)?;
        let Description::Timer(timerfd) = &*description else {
            return Err(EINVAL);
        };
        let now = clock::monotonic()?;
        // SAFETY: musl passes a valid `struct itimerspec`.
        let spec = unsafe { new.read() };
        let arming = Arming::from_spec(&spec, flags & TFD_TIMER_ABSTIME != 0, now)?;
        let mut current = timerfd.arming.lock();
        if !old.is_null() {
            // SAFETY: musl passes a valid `struct itimerspec` to fill.
            unsafe { old.write(current.spec(now)) };
        }
        timerfd.set(arming)?;
        *current = arming;
        Ok(0)
    })())
}

/// `timerfd_gettime`: the time until the next expiration of the timer of
/// `fd`, zero when it is disarmed, and its period, in `out`.
pub fn gettime(fd: i64, out: *mut Itimerspec) -> i64 {
    result((|| {
        if out.is_null() {
            return Err(EFAULT);
        }
        let description = timer(fd)?;
        let Description::Timer(timerfd) = &*description else {
            return Err(EINVAL);
        };
        let now = clock::monotonic()?;
        let spec = timerfd.arming.lock().spec(now);
        // SAFETY: musl passes a valid `struct itimerspec` to fill.
        unsafe { out.write(spec) };
        Ok(0)
    })())
}

/// A read of `len` bytes into `buf` from `timerfd`: the expirations since
/// the last read, as eight bytes, once there is at least one. It waits
/// for one, or fails with `EAGAIN` when the descriptor does not wait.
pub fn read(timerfd: &Timerfd, buf: *mut u8, len: u64) -> i64 {
    if len < 8 {
        return -EINVAL;
    }
    result((|| {
        loop {
            {
                let now = clock::monotonic()?;
                let mut arming = timerfd.arming.lock();
                let (count, after) = arming.expire(now);
                if count > 0 {
                    timerfd.set(after)?;
                    *arming = after;
                    // SAFETY: musl passes a buffer of `len` bytes, at least
                    // eight.
                    unsafe { buf.cast::<u64>().write_unaligned(count) };
                    return Ok(8);
                }
            }
            if timerfd.flags.load(Ordering::Relaxed) & O_NONBLOCK != 0 {
                return Err(EAGAIN);
            }
            let mut observed = 0;
            // SAFETY: the out pointer is valid for the call.
            let status = unsafe {
                sys::object_wait_one(timerfd.timer.raw(), SIGNALED, FOREVER, &mut observed)
            };
            if status != mikros_rt::OK {
                return Err(errno::from_status(status));
            }
        }
    })())
}
