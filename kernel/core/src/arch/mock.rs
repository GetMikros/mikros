//! A mock of the architecture interface for the development host.
//!
//! It models one processor. Its state is global to the process, so tests
//! that depend on it must not run at the same time; in host tests `lock`
//! serializes them. Only the per-processor data pointer is kept per test
//! thread in host tests. Operations that only make sense on real hardware, such as
//! switching stacks or entering user mode, panic.

use super::Arch;
use crate::sync::SpinLock;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, Ordering};

/// The value of `TIMER` while the timer is not armed.
const DISARMED: i64 = i64::MIN;

static INTERRUPTS: AtomicBool = AtomicBool::new(false);
// In host tests each test thread is a processor of its own, so that a
// test that sets the per-processor data does not change it for others.
#[cfg(test)]
std::thread_local! {
    static CPU_DATA: AtomicPtr<()> = const { AtomicPtr::new(core::ptr::null_mut()) };
}
#[cfg(not(test))]
static CPU_DATA: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Runs `f` on this processor's data pointer.
fn with_cpu_data<R>(f: impl FnOnce(&AtomicPtr<()>) -> R) -> R {
    #[cfg(test)]
    return CPU_DATA.with(f);
    #[cfg(not(test))]
    f(&CPU_DATA)
}
static NOW: AtomicI64 = AtomicI64::new(0);
static TIMER: AtomicI64 = AtomicI64::new(DISARMED);
static OUTPUT: SpinLock<Vec<u8>> = SpinLock::new(Vec::new());

/// The mock architecture.
pub struct Mock;

/// Serializes the tests that use the mock's state, and resets that state.
/// The state stays reset until the guard is dropped, apart from what the
/// test itself changes. A standard mutex, so that holding it does not
/// count as holding a kernel lock.
#[cfg(test)]
pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    static TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = TESTS.lock().unwrap_or_else(|e| e.into_inner());
    INTERRUPTS.store(false, Ordering::Relaxed);
    with_cpu_data(|d| d.store(core::ptr::null_mut(), Ordering::Relaxed));
    NOW.store(0, Ordering::Relaxed);
    TIMER.store(DISARMED, Ordering::Relaxed);
    OUTPUT.lock().clear();
    guard
}

/// Sets the monotonic clock.
pub fn set_now(now: i64) {
    NOW.store(now, Ordering::Relaxed);
}

/// The deadline the timer is armed for, if any.
pub fn timer_deadline() -> Option<i64> {
    Some(TIMER.load(Ordering::Relaxed)).filter(|&t| t != DISARMED)
}

/// Takes what has been written to the debug output.
pub fn take_output() -> Vec<u8> {
    core::mem::take(&mut *OUTPUT.lock())
}

impl Arch for Mock {
    fn halt() -> ! {
        panic!("the mock architecture cannot halt");
    }

    fn wait_for_interrupt() {
        assert!(
            !INTERRUPTS.load(Ordering::Relaxed),
            "waited for an interrupt with interrupts enabled"
        );
    }

    fn interrupts_enabled() -> bool {
        INTERRUPTS.load(Ordering::Relaxed)
    }

    fn interrupts_disable() -> bool {
        INTERRUPTS.swap(false, Ordering::Relaxed)
    }

    unsafe fn interrupts_restore(enabled: bool) {
        if enabled {
            INTERRUPTS.store(true, Ordering::Relaxed);
        }
    }

    fn handling_interrupt() -> bool {
        false
    }

    fn cpu_index() -> u32 {
        0
    }

    fn cpu_data() -> *const () {
        with_cpu_data(|d| d.load(Ordering::Acquire))
    }

    unsafe fn set_cpu_data(data: *const ()) {
        with_cpu_data(|d| d.store(data.cast_mut(), Ordering::Release));
    }

    fn monotonic_now() -> i64 {
        NOW.load(Ordering::Relaxed)
    }

    fn clock_maintenance_interval() -> Option<i64> {
        None
    }

    fn clock_maintain() {}

    fn timer_arm(deadline: i64) {
        TIMER.store(deadline.max(DISARMED + 1), Ordering::Relaxed);
    }

    fn timer_disarm() {
        TIMER.store(DISARMED, Ordering::Relaxed);
    }

    fn kernel_pages_alloc(size: u64) -> Option<u64> {
        if size == 0 {
            return None;
        }
        let layout =
            core::alloc::Layout::from_size_align(usize::try_from(size).ok()?, 4096).ok()?;
        // SAFETY: the layout has a nonzero size.
        let start = unsafe { alloc::alloc::alloc_zeroed(layout) };
        (!start.is_null()).then_some(start as u64)
    }

    unsafe fn kernel_pages_free(start: u64, size: u64) {
        let layout = core::alloc::Layout::from_size_align(size as usize, 4096)
            .expect("the layout of an allocation");
        // SAFETY: the caller passes an allocation of this layout.
        unsafe { alloc::alloc::dealloc(start as *mut u8, layout) };
    }

    fn debug_write(bytes: &[u8]) {
        let mut output = OUTPUT.lock();
        if output.try_reserve(bytes.len()).is_ok() {
            output.extend_from_slice(bytes);
        }
    }

    unsafe fn context_prepare(_stack_top: u64, _entry: u64, _arg: u64) -> u64 {
        panic!("the mock architecture cannot prepare kernel stacks");
    }

    unsafe fn context_switch(_save: *mut u64, _load: u64) {
        panic!("the mock architecture cannot switch kernel stacks");
    }

    fn extended_state_layout() -> (usize, usize) {
        (64, 64)
    }

    unsafe fn extended_state_init(_area: *mut u8) {}

    unsafe fn extended_state_save(_area: *mut u8) {}

    unsafe fn extended_state_restore(_area: *const u8) {}

    unsafe fn enter_user(_entry: u64, _stack: u64, _arg1: u64, _arg2: u64) -> ! {
        panic!("the mock architecture cannot enter user mode");
    }
}
