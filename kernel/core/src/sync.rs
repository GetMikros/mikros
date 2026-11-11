//! A spin lock.
//!
//! The lock does not disable interrupts itself: kernel code runs with
//! interrupts disabled, and enables them only at preemption points and
//! while idle, where no lock is held. So every lock, including one that
//! interrupt handlers take, is held with interrupts disabled. A thread
//! never switches away with a lock held; `sched` asserts both.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "diag")]
use core::{panic::Location, ptr, sync::atomic::AtomicPtr};

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
    /// The class that orders the lock among the kernel's locks.
    #[cfg_attr(not(lock_check), allow(dead_code))]
    class: crate::lock_order::Class,
    /// In the diagnostic build, where the lock was last taken, or null.
    #[cfg(feature = "diag")]
    taken_at: AtomicPtr<Location<'static>>,
    value: UnsafeCell<T>,
}

// SAFETY: the lock gives one thread at a time access to the value, so the
// value only needs to be sendable between threads.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// A lock of class `class` around `value`.
    pub const fn new(class: crate::lock_order::Class, value: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            class,
            #[cfg(feature = "diag")]
            taken_at: AtomicPtr::new(ptr::null_mut()),
            value: UnsafeCell::new(value),
        }
    }

    #[cfg_attr(feature = "diag", track_caller)]
    pub fn lock(&self) -> Guard<'_, T> {
        #[cfg(lock_check)]
        crate::lock_order::acquire(self.class, crate::lock_order::Check::Order);
        self.spin()
    }

    /// Takes the lock while another lock of its class may be held. The
    /// caller documents the order among the locks of the class that makes
    /// this safe.
    #[cfg_attr(feature = "diag", track_caller)]
    pub fn lock_nested(&self) -> Guard<'_, T> {
        #[cfg(lock_check)]
        crate::lock_order::acquire(self.class, crate::lock_order::Check::Nested);
        self.spin()
    }

    #[cfg_attr(feature = "diag", track_caller)]
    fn spin(&self) -> Guard<'_, T> {
        #[cfg(feature = "diag")]
        let mut wait = Wait::default();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            crate::arch::spin_wait();
            core::hint::spin_loop();
            #[cfg(feature = "diag")]
            wait.turn(self);
        }
        #[cfg(feature = "diag")]
        self.taken();
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

    /// In the diagnostic build, records where the lock was taken.
    #[cfg(feature = "diag")]
    #[track_caller]
    fn taken(&self) {
        let at: &'static Location<'static> = Location::caller();
        self.taken_at
            .store(ptr::from_ref(at).cast_mut(), Ordering::Relaxed);
    }

    /// The lock, if it is free now.
    #[cfg_attr(feature = "diag", track_caller)]
    pub fn try_lock(&self) -> Option<Guard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| {
                // Trying cannot deadlock, but what is taken while it is
                // held must still come after it.
                #[cfg(lock_check)]
                crate::lock_order::acquire(self.class, crate::lock_order::Check::None);
                #[cfg(feature = "diag")]
                self.taken();
                crate::cpu::this()
                    .locks_held
                    .fetch_add(1, Ordering::Relaxed);
                Guard { lock: self }
            })
    }
}

/// In the diagnostic build, how long a thread has waited for a lock,
/// which the monotonic clock tells every `Wait::TURNS` turns.
#[cfg(feature = "diag")]
#[derive(Default)]
struct Wait {
    turns: u32,
    since: Option<i64>,
}

#[cfg(feature = "diag")]
impl Wait {
    const TURNS: u32 = 1024;
    /// The longest wait, in nanoseconds.
    const LIMIT: i64 = 5_000_000_000;

    /// Counts a turn of waiting for `lock`, and stops the kernel once
    /// the wait has lasted `LIMIT`.
    fn turn<T>(&mut self, lock: &SpinLock<T>) {
        self.turns += 1;
        if self.turns % Self::TURNS != 0 {
            return;
        }
        let now = crate::arch::monotonic_now();
        let since = *self.since.get_or_insert(now);
        if now.saturating_sub(since) < Self::LIMIT {
            return;
        }
        let at = lock.taken_at.load(Ordering::Relaxed);
        // SAFETY: `taken` stores only `&'static Location`s.
        match unsafe { at.as_ref() } {
            Some(at) => panic!(
                "waited 5 s for the spin lock of class {}, which its holder took at {at}",
                lock.class.name()
            ),
            None => panic!(
                "waited 5 s for the spin lock of class {}, which was never taken",
                lock.class.name()
            ),
        }
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
        #[cfg(lock_check)]
        crate::lock_order::release(self.lock.class);
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
        let counter = SpinLock::new(crate::lock_order::TEST, 0u64);
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

    #[cfg(feature = "diag")]
    #[test]
    fn names_where_a_lock_waited_for_too_long_was_taken() {
        let _mock = crate::arch::mock::lock();
        crate::arch::mock::set_now(0);
        let lock = SpinLock::new(crate::lock_order::TEST, ());
        let (held, line) = (lock.lock(), line!());
        std::thread::scope(|s| {
            let waiter = s.spawn(|| drop(lock.lock()));
            // The waiter reads the clock after its first turns.
            std::thread::sleep(std::time::Duration::from_millis(100));
            crate::arch::mock::set_now(Wait::LIMIT);
            let panic = waiter.join().unwrap_err();
            let message = panic.downcast_ref::<String>().unwrap();
            assert!(message.starts_with("waited 5 s for the spin lock of class TEST"));
            assert!(message.contains(&format!("sync.rs:{line}:")), "{message}");
        });
        drop(held);
    }
}
