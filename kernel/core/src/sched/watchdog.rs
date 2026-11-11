//! The lockup watchdog of the diagnostic build. Every processor's timer
//! interrupts it at least every `HEARTBEAT`. At each interrupt the
//! processor records the time and whether threads wait in its queue,
//! and checks every processor: the kernel stops when one has not handled
//! its timer interrupt for `LIMIT`, or when no user thread has run for
//! `LIMIT` while one waited in a queue.

use super::{CpuSched, Thread};
use core::sync::atomic::{AtomicI64, Ordering};

/// The longest a processor's timer stays unarmed.
pub const HEARTBEAT: i64 = 1_000_000_000;
/// How long a processor may go without its timer interrupt, and user
/// threads without running while one is runnable.
const LIMIT: i64 = 10_000_000_000;

/// When a user thread last ran on any processor.
static USER_RAN: AtomicI64 = AtomicI64::new(0);

/// Records the thread that this processor runs from `now` on, or that it
/// idles.
pub fn started(thread: Option<&Thread>, now: i64) {
    let koid = thread.map_or(0, |t| t.header.koid());
    crate::cpu::this().running.store(koid, Ordering::Relaxed);
    if thread.is_some_and(|t| t.process().is_some()) {
        USER_RAN.fetch_max(now, Ordering::Relaxed);
    }
}

/// Handles this processor's timer interrupt at `now`, with its scheduler
/// state `sched`, and stops the kernel when a check fails.
pub fn tick(now: i64, sched: &CpuSched) {
    let me = crate::cpu::this();
    me.last_tick.store(now, Ordering::Relaxed);
    if sched
        .current
        .as_ref()
        .is_some_and(|t| t.process().is_some())
    {
        USER_RAN.fetch_max(now, Ordering::Relaxed);
    }
    if sched.fair.is_empty() {
        me.waiting_since.store(0, Ordering::Relaxed);
    } else if me.waiting_since.load(Ordering::Relaxed) == 0 {
        me.waiting_since.store(now, Ordering::Relaxed);
    }
    if let Some(failed) = check(now) {
        panic!("watchdog: {failed}");
    }
}

/// A failed check of the watchdog.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// A processor and how long it went without its timer interrupt.
    NoTimer(usize, i64),
    /// How long no user thread ran, and a processor with waiting threads.
    NoUserThread(i64, usize),
}

impl core::fmt::Display for Failed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Failed::NoTimer(cpu, ns) => write!(
                f,
                "processor {cpu} has not handled its timer interrupt for {} ms",
                ns / 1_000_000
            ),
            Failed::NoUserThread(ns, cpu) => write!(
                f,
                "no user thread has run for {} ms while processor {cpu} had runnable threads",
                ns / 1_000_000
            ),
        }
    }
}

/// The first failed check at `now`, over every processor.
fn check(now: i64) -> Option<Failed> {
    let ran = USER_RAN.load(Ordering::Relaxed);
    (0..crate::cpu::count()).find_map(|index| {
        let cpu = crate::cpu::of(index)?;
        let tick = cpu.last_tick.load(Ordering::Relaxed);
        if tick != 0 && now.saturating_sub(tick) >= LIMIT {
            return Some(Failed::NoTimer(index, now - tick));
        }
        let waiting = cpu.waiting_since.load(Ordering::Relaxed);
        let idle = now.saturating_sub(waiting.max(ran));
        (waiting != 0 && idle >= LIMIT).then_some(Failed::NoUserThread(now - ran, index))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_processor_without_its_timer_and_threads_left_waiting() {
        let _mock = crate::arch::mock::lock();
        let cpu = crate::cpu::of(0).unwrap();
        USER_RAN.store(1, Ordering::Relaxed);
        cpu.last_tick.store(5, Ordering::Relaxed);
        cpu.waiting_since.store(0, Ordering::Relaxed);
        assert_eq!(check(5 + LIMIT - 1), None);
        assert_eq!(check(5 + LIMIT), Some(Failed::NoTimer(0, LIMIT)));
        cpu.last_tick.store(3 * LIMIT, Ordering::Relaxed);
        cpu.waiting_since.store(2 * LIMIT, Ordering::Relaxed);
        USER_RAN.store(2 * LIMIT + 7, Ordering::Relaxed);
        assert_eq!(check(3 * LIMIT), None);
        assert_eq!(check(3 * LIMIT + 7), Some(Failed::NoUserThread(LIMIT, 0)));
        cpu.last_tick.store(0, Ordering::Relaxed);
        cpu.waiting_since.store(0, Ordering::Relaxed);
        USER_RAN.store(0, Ordering::Relaxed);
    }
}
