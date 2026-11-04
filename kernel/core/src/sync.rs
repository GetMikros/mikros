//! A spin lock.
//!
//! The lock does not disable interrupts. Code that takes it must not be
//! interrupted by code that takes it too.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Whether the running thread holds no spin lock.
pub fn none_held() -> bool {
    crate::cpu::this().locks_held.load(Ordering::Relaxed) == 0
}

/// The number of objects destroyed between reschedule checks.
pub const DESTROY_STEP: usize = 512;

/// The scheduler's preemption point, as a `fn()`, or 0 before it is set.
static PREEMPT_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Installs `hook`, which checks for a pending reschedule and lets other
/// threads run, as the preemption point of `destroyed`.
pub fn set_preempt_hook(hook: fn()) {
    PREEMPT_HOOK.store(hook as usize, Ordering::Release);
}

/// Accounts for destroying `count` objects. Once `DESTROY_STEP` have been
/// destroyed on this processor since the last check, checks for a
/// reschedule through the preemption point when no lock is held. The
/// count is shared by nested destruction, so a chain of objects that each
/// destroy a few more is checked too.
pub fn destroyed(count: usize) {
    let cpu = crate::cpu::this();
    let total = cpu.destroyed.fetch_add(count, Ordering::Relaxed) + count;
    if total < DESTROY_STEP || !none_held() {
        return;
    }
    cpu.destroyed.store(0, Ordering::Relaxed);
    let hook = PREEMPT_HOOK.load(Ordering::Acquire);
    if hook != 0 {
        // SAFETY: only `set_preempt_hook` stores a nonzero value, which
        // is a `fn()`.
        let hook: fn() = unsafe { core::mem::transmute(hook) };
        hook();
    }
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
        crate::cpu::this()
            .locks_held
            .fetch_add(1, Ordering::Relaxed);
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
                crate::cpu::this()
                    .locks_held
                    .fetch_add(1, Ordering::Relaxed);
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
        crate::cpu::this()
            .locks_held
            .fetch_sub(1, Ordering::Relaxed);
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
