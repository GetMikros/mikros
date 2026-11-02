//! Seeding the kernel random number generator on x86_64.

use crate::cpu::rdtsc;
use crate::log::log;
use boot_protocol::{BootInfo, flags::RNG_SEED_VALID};
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use kernel_core::deadline::INFINITE;
use kernel_core::random::{Error, Rng};
use kernel_core::sync::SpinLock;

static RNG: SpinLock<Rng> = SpinLock::new(Rng::new());

/// The words taken from each hardware source per reseed.
const WORDS: usize = 4;
/// How often a failed instruction is retried before the word is skipped.
const TRIES: u32 = 10;

/// How long after a successful hardware reseed the next is due: 300
/// seconds less room for a late timer interrupt and the reseed itself.
const RESEED_INTERVAL: i64 = 290_000_000_000;
/// How long after a failed reseed it is tried again.
const RESEED_RETRY: i64 = 1_000_000_000;

/// Whether CPUID reports a hardware source.
static HARDWARE: AtomicBool = AtomicBool::new(false);
/// When the last successful hardware reseed, or the boot seeding, ended.
static LAST_RESEED: AtomicI64 = AtomicI64::new(0);

#[derive(Clone, Copy)]
enum Source {
    Rdseed,
    Rdrand,
}

fn reported(source: Source) -> bool {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    match source {
        Source::Rdseed => __cpuid(0).eax >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0,
        Source::Rdrand => __cpuid(1).ecx & (1 << 30) != 0,
    }
}

/// One word from `source`, or `None` when the instruction reports a
/// failure.
fn word(source: Source) -> Option<u64> {
    let value: u64;
    let ok: u8;
    // SAFETY: the instruction is reported by CPUID; it only writes the
    // output register and the flags.
    unsafe {
        match source {
            Source::Rdseed => {
                asm!("rdseed {}", "setc {}", out(reg) value, out(reg_byte) ok, options(nomem, nostack))
            }
            Source::Rdrand => {
                asm!("rdrand {}", "setc {}", out(reg) value, out(reg_byte) ok, options(nomem, nostack))
            }
        }
    }
    (ok != 0).then_some(value)
}

/// Fills `out` with words from `source` and returns how many bytes of it
/// hold output. Failed instructions contribute nothing.
fn read(source: Source, out: &mut [u8; WORDS * 8]) -> usize {
    if !reported(source) {
        return 0;
    }
    let mut len = 0;
    for _ in 0..WORDS {
        if let Some(w) = (0..TRIES).find_map(|_| word(source)) {
            out[len..len + 8].copy_from_slice(&w.to_le_bytes());
            len += 8;
        }
    }
    len
}

/// Mixes output of every hardware source, and `extra`, into the
/// generator. Returns whether any hardware source gave output.
fn reseed_with(extra: &[u8]) -> bool {
    let mut seed = [0u8; WORDS * 8];
    let mut rand = [0u8; WORDS * 8];
    let seed_len = read(Source::Rdseed, &mut seed);
    let rand_len = read(Source::Rdrand, &mut rand);
    RNG.lock()
        .reseed(&[extra, &seed[..seed_len], &rand[..rand_len]]);
    seed.fill(0);
    rand.fill(0);
    seed_len + rand_len > 0
}

/// When the next hardware reseed is due, or `INFINITE` without a
/// hardware source.
pub fn reseed_due() -> i64 {
    if HARDWARE.load(Ordering::Relaxed) {
        LAST_RESEED
            .load(Ordering::Relaxed)
            .saturating_add(RESEED_INTERVAL)
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
        LAST_RESEED.store(crate::time::now(), Ordering::Relaxed);
        reseed_due()
    } else {
        now.saturating_add(RESEED_RETRY)
    }
}

/// TSC differences across short busy loops, whose timing varies with
/// caches, interrupts and the memory system.
fn jitter(out: &mut [u8; 2048]) {
    for byte in out.iter_mut() {
        let start = rdtsc();
        for i in 0..64u32 {
            core::hint::black_box(i);
        }
        *byte = (rdtsc() - start) as u8;
    }
}

/// Seeds the generator from the firmware seed and the hardware sources,
/// or from timer measurements when there are neither.
pub fn init(info: &BootInfo) {
    let firmware = info.flags & RNG_SEED_VALID != 0;
    let seed: &[u8] = if firmware { &info.rng_seed } else { &[] };
    let hardware = reseed_with(seed);
    HARDWARE.store(
        reported(Source::Rdseed) || reported(Source::Rdrand),
        Ordering::Relaxed,
    );
    LAST_RESEED.store(crate::time::now(), Ordering::Relaxed);
    if !firmware && !hardware {
        let mut samples = [0u8; 2048];
        jitter(&mut samples);
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
