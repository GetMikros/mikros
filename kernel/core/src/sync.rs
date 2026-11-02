//! A spin lock.
//!
//! The lock does not disable interrupts. Code that takes it must not be
//! interrupted by code that takes it too.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The number of spin locks held. The kernel runs one thread at a time on
/// one processor and switches threads only where no lock is held, so
/// this counts the locks of the running thread.
static HELD: AtomicUsize = AtomicUsize::new(0);

/// Whether no spin lock is held.
pub fn none_held() -> bool {
    HELD.load(Ordering::Relaxed) == 0
}

pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock gives one thread at a time access to the value, so the
// value only needs to be sendable between threads.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> Guard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        HELD.fetch_add(1, Ordering::Relaxed);
        Guard { lock: self }
    }

    /// The value, which no one else can reach through a unique reference,
    /// without taking the lock.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// The lock, if it is free now.
    pub fn try_lock(&self) -> Option<Guard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| {
                HELD.fetch_add(1, Ordering::Relaxed);
                Guard { lock: self }
            })
    }
}

pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard exists only while this thread holds the lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard exists only while this thread holds the lock.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        HELD.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_access() {
        let counter = SpinLock::new(0u64);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..10_000 {
                        *counter.lock() += 1;
                    }
                });
            }
        });
        assert_eq!(*counter.lock(), 40_000);
    }
}
