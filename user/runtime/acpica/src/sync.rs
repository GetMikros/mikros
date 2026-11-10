//! The locks, mutexes and semaphores of ACPICA's operating system layer,
//! on futexes, waiting with timeouts on the monotonic clock.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::sys;

pub const FOREVER: i64 = i64::MAX;

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

pub fn wait(word: &AtomicU32, seen: u32, deadline: i64) {
    // SAFETY: the word stays valid while the thread waits on it.
    unsafe { sys::futex_wait(word.as_ptr(), seen, deadline) };
}

pub fn wake(word: &AtomicU32, count: u32) {
    let mut woken = 0;
    // SAFETY: the word stays valid while threads wait on it.
    unsafe { sys::futex_wake(word.as_ptr(), count, &mut woken) };
}

/// A count of units that waiters take and signalers give back.
pub struct Semaphore {
    units: AtomicU32,
    max: u32,
}

impl Semaphore {
    pub const fn new(max: u32, initial: u32) -> Semaphore {
        Semaphore {
            units: AtomicU32::new(initial),
            max,
        }
    }

    /// Takes `units`, waiting until `deadline` for them: whether it took
    /// them.
    pub fn take(&self, units: u32, deadline: i64) -> bool {
        loop {
            let have = self.units.load(Ordering::Acquire);
            if have >= units {
                if self
                    .units
                    .compare_exchange(have, have - units, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return true;
                }
                continue;
            }
            if now() >= deadline {
                return false;
            }
            wait(&self.units, have, deadline);
        }
    }

    /// Gives back `units`: false when that would make more than the most.
    pub fn give(&self, units: u32) -> bool {
        let mut have = self.units.load(Ordering::Acquire);
        loop {
            let Some(after) = have.checked_add(units).filter(|&a| a <= self.max) else {
                return false;
            };
            match self
                .units
                .compare_exchange(have, after, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(now) => have = now,
            }
        }
        wake(&self.units, u32::MAX);
        true
    }
}

/// A lock that one thread holds at a time, waiting for it until a
/// deadline; ACPICA's mutexes and spinlocks both. Its word is 0 when it
/// is free, 1 when it is held, and 2 when it is held and threads may be
/// waiting for it, so that giving it back only wakes when it has to.
pub struct Lock {
    state: AtomicU32,
}

impl Lock {
    pub const fn new() -> Lock {
        Lock {
            state: AtomicU32::new(0),
        }
    }

    /// Takes the lock, waiting until `deadline`: whether it took it.
    pub fn take(&self, deadline: i64) -> bool {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
        loop {
            if self.state.swap(2, Ordering::Acquire) == 0 {
                return true;
            }
            if now() >= deadline {
                return false;
            }
            wait(&self.state, 2, deadline);
        }
    }

    pub fn give(&self) {
        if self.state.swap(0, Ordering::Release) == 2 {
            wake(&self.state, 1);
        }
    }
}

/// A value that one thread uses at a time, behind a [`Lock`].
pub struct Locked<T> {
    lock: Lock,
    value: UnsafeCell<T>,
}

// SAFETY: the value is only reached through a guard, and the lock lets
// one guard exist at a time.
unsafe impl<T: Send> Sync for Locked<T> {}

impl<T> Locked<T> {
    pub const fn new(value: T) -> Locked<T> {
        Locked {
            lock: Lock::new(),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> Guard<'_, T> {
        self.lock.take(FOREVER);
        Guard { locked: self }
    }
}

pub struct Guard<'a, T> {
    locked: &'a Locked<T>,
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.locked.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock, and this is its only mutable
        // borrow.
        unsafe { &mut *self.locked.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.locked.lock.give();
    }
}
