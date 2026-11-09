//! The x86_64 random sources: RDSEED and RDRAND.

use core::arch::asm;

/// The words taken from each hardware source per reseed.
const WORDS: usize = 4;
/// How often a failed instruction is retried before the word is skipped.
const TRIES: u32 = 10;

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
    // The fuzz build's `fuzz_fail_random` makes an instruction report
    // failure, leaving its result as it is.
    #[cfg(feature = "fuzz")]
    let ok = match source {
        Source::Rdseed if crate::fuzz::random_fails(crate::fuzz::RDSEED) => 0,
        Source::Rdrand if crate::fuzz::random_fails(crate::fuzz::RDRAND) => 0,
        _ => ok,
    };
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

/// Whether CPUID reports RDSEED or RDRAND.
pub fn present() -> bool {
    reported(Source::Rdseed) || reported(Source::Rdrand)
}

/// Fills the start of `out` with words from RDSEED, then RDRAND, and
/// returns how many bytes it filled. `out` holds at least 64 bytes.
pub fn seed(out: &mut [u8]) -> usize {
    let mut len = 0;
    for source in [Source::Rdseed, Source::Rdrand] {
        let mut words = [0u8; WORDS * 8];
        let got = read(source, &mut words);
        let got = got.min(out.len() - len);
        out[len..len + got].copy_from_slice(&words[..got]);
        words.fill(0);
        len += got;
    }
    len
}
