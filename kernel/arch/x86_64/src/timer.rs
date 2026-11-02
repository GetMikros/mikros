//! The local APIC timer.

use crate::apic::{self, LVT_TIMER, TIMER_VECTOR};
use crate::cpu::{rdtsc, write_msr};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const IA32_TSC_DEADLINE: u32 = 0x6E0;

/// Timer registers, by their xAPIC offset.
const INITIAL_COUNT: u32 = 0x380;
const CURRENT_COUNT: u32 = 0x390;
const DIVIDE: u32 = 0x3E0;

const DIVIDE_BY_1: u32 = 0b1011;
const MODE_TSC_DEADLINE: u32 = 0b10 << 17;
const LVT_MASKED: u32 = 1 << 16;

static DEADLINE_MODE: AtomicBool = AtomicBool::new(false);
/// The timer's count rate in one-shot mode.
static HZ: AtomicU64 = AtomicU64::new(0);

/// Whether CPUID reports TSC-deadline mode.
pub fn tsc_deadline() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 24) != 0
}

/// Starts the masked timer counting down from its largest count, so that
/// `count` can be measured.
pub fn start_count() {
    apic::write(LVT_TIMER, LVT_MASKED | u32::from(TIMER_VECTOR));
    apic::write(DIVIDE, DIVIDE_BY_1);
    apic::write(INITIAL_COUNT, u32::MAX);
}

/// The ticks counted since `start_count`.
pub fn count() -> u64 {
    u64::from(u32::MAX - apic::read(CURRENT_COUNT))
}

/// Records the measured one-shot count rate.
pub fn set_hz(hz: u64) {
    HZ.store(hz, Ordering::Relaxed);
}

/// Puts the timer in TSC-deadline mode when CPUID reports it and in
/// one-shot mode otherwise, unmasked and stopped. The clock must be set
/// up.
pub fn init() {
    let deadline = tsc_deadline();
    DEADLINE_MODE.store(deadline, Ordering::Relaxed);
    apic::write(INITIAL_COUNT, 0);
    let mode = if deadline { MODE_TSC_DEADLINE } else { 0 };
    apic::write(LVT_TIMER, mode | u32::from(TIMER_VECTOR));
    apic::write(DIVIDE, DIVIDE_BY_1);
    // A write to IA32_TSC_DEADLINE must not pass the mode change.
    // SAFETY: MFENCE only orders memory accesses.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
}

/// The number of ticks at `hz` in `ns` nanoseconds.
fn ticks(ns: u64, hz: u64) -> u64 {
    (u128::from(ns) * u128::from(hz) / 1_000_000_000).min(u128::from(u64::MAX)) as u64
}

/// Arms the timer to interrupt at the monotonic time `deadline`, replacing
/// any earlier setting. A deadline that has passed interrupts at once. In
/// one-shot mode, a deadline beyond the timer's range interrupts at the
/// end of the range, before the deadline.
pub fn arm(deadline: i64) {
    let wait = u64::try_from(deadline.saturating_sub(crate::time::now())).unwrap_or(0);
    if DEADLINE_MODE.load(Ordering::Relaxed) {
        let at = rdtsc().saturating_add(ticks(wait, crate::time::tsc_hz()).max(1));
        // SAFETY: the MSR exists in TSC-deadline mode, and writing it only
        // sets when the timer interrupts.
        unsafe { write_msr(IA32_TSC_DEADLINE, at) };
    } else {
        let count = ticks(wait, HZ.load(Ordering::Relaxed)).clamp(1, u64::from(u32::MAX));
        apic::write(INITIAL_COUNT, count as u32);
    }
}

/// Handles the timer interrupt.
pub fn interrupt() {
    apic::eoi();
    crate::sched::timer_interrupt();
}
