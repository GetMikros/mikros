//! The kernel random number generator, seeded from the firmware and the
//! processor's random source, or else from timer measurements.

use crate::log::log;
use boot_protocol::BootInfo;
use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use kernel_core::deadline::INFINITE;
use kernel_core::random::{Error, RESEED_INTERVAL, RESEED_RETRY, Rng};
use kernel_core::sync::SpinLock;

static RNG: SpinLock<Rng> = SpinLock::new(Rng::new());

/// The most seed bytes the processor's random source gives per reseed.
const SEED_SIZE: usize = 64;

/// Whether CPUID reports a hardware source.
static HARDWARE: AtomicBool = AtomicBool::new(false);
/// When the last successful hardware reseed, or the boot seeding, ended.
static LAST_RESEED: AtomicI64 = AtomicI64::new(0);

/// Mixes output of every hardware source, and `extra`, into the
/// generator. Returns whether any hardware source gave output.
fn reseed_with(extra: &[u8]) -> bool {
    let mut seed = [0u8; SEED_SIZE];
    let len = kernel_core::arch::random_seed(&mut seed);
    RNG.lock().reseed(&[extra, &seed[..len]]);
    seed.fill(0);
    len > 0
}

/// The time from a successful hardware reseed to the next, and from a
/// failed one to the next try. The fuzz build's `fuzz_set_interval` can
/// shorten both.
fn intervals() -> (i64, i64) {
    #[cfg(feature = "fuzz")]
    if let Some(ns) = crate::fuzz::interval(crate::fuzz::RESEED) {
        return (ns, ns);
    }
    (RESEED_INTERVAL, RESEED_RETRY)
}

/// When the next hardware reseed is due, or `INFINITE` without a
/// hardware source.
pub fn reseed_due() -> i64 {
    if HARDWARE.load(Ordering::Relaxed) {
        LAST_RESEED
            .load(Ordering::Relaxed)
            .saturating_add(intervals().0)
    } else {
        INFINITE
    }
}

/// Mixes new output of the hardware sources into the generator for a
/// reseed deadline reached at `now`, and returns when the next is due:
/// after the interval once the reseed succeeds, soon when no source gave
/// output.
pub fn reseed(now: i64) -> i64 {
    if reseed_with(&[]) {
        LAST_RESEED.store(kernel_core::arch::monotonic_now(), Ordering::Relaxed);
        reseed_due()
    } else {
        now.saturating_add(intervals().1)
    }
}

/// When the last successful hardware reseed, or the boot seeding, ended.
#[cfg(feature = "fuzz")]
pub fn last_reseed() -> i64 {
    LAST_RESEED.load(Ordering::Relaxed)
}

/// Seeds the generator from the firmware seed and the hardware sources,
/// or from timer measurements when there are neither.
pub fn init(info: &BootInfo) {
    let firmware = info.firmware_seed();
    let seed: &[u8] = firmware.map_or(&[], |s| s.as_slice());
    let hardware = reseed_with(seed);
    HARDWARE.store(
        kernel_core::arch::random_source_present(),
        Ordering::Relaxed,
    );
    LAST_RESEED.store(kernel_core::arch::monotonic_now(), Ordering::Relaxed);
    if firmware.is_none() && !hardware {
        // Cycle counts across short busy loops, whose timing varies with
        // caches, interrupts and the memory system.
        let mut samples = [0u8; 2048];
        for byte in samples.iter_mut() {
            let start = kernel_core::arch::cycle_counter();
            for i in 0..64u32 {
                core::hint::black_box(i);
            }
            *byte = (kernel_core::arch::cycle_counter() - start) as u8;
        }
        RNG.lock().reseed(&[&samples]);
        log!(
            Warning,
            "no firmware or hardware random source; the random number generator is seeded from timer measurements"
        );
    }
}

/// Fills `out` with random bytes.
pub fn fill(out: &mut [u8]) -> Result<(), Error> {
    RNG.lock().fill(out)
}
