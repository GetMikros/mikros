//! Turning hardware counter values into the monotonic clock.

use core::sync::atomic::{AtomicU64, Ordering};

/// Nanoseconds for `ticks` of a counter running at `hz`.
pub fn ticks_to_ns(ticks: u64, hz: u64) -> i64 {
    let ns = u128::from(ticks) * 1_000_000_000 / u128::from(hz.max(1));
    i64::try_from(ns).unwrap_or(i64::MAX)
}

/// Extends a counter of `bits` bits that wraps around to 64 bits. A value
/// is only extended correctly when the counter has wrapped at most once
/// since the previous value was extended.
pub struct Extender {
    bits: u32,
    last: AtomicU64,
}

impl Extender {
    pub const fn new(bits: u32) -> Self {
        Extender {
            bits,
            last: AtomicU64::new(0),
        }
    }

    pub fn extend(&self, raw: u64) -> u64 {
        if self.bits >= 64 {
            return raw;
        }
        let mask = (1u64 << self.bits) - 1;
        let raw = raw & mask;
        let mut last = self.last.load(Ordering::Relaxed);
        loop {
            let mut value = (last & !mask) | raw;
            if value < last {
                value += mask + 1;
            }
            match self
                .last
                .compare_exchange_weak(last, value, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return value,
                // Another reader extended a value at least as recent.
                Err(newer) if newer >= value => return newer.max(value),
                Err(newer) => last = newer,
            }
        }
    }
}

/// A quarter of the wrap period, in nanoseconds, of a counter of `width`
/// bits, below 64, that counts at `hz`.
pub fn quarter_wrap_ns(width: u32, hz: u64) -> i64 {
    let wrap_ns = (1u128 << width) * 1_000_000_000 / u128::from(hz.max(1));
    i64::try_from(wrap_ns / 4).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_ticks() {
        assert_eq!(ticks_to_ns(3_579_545, 3_579_545), 1_000_000_000);
        // A 24-bit PM timer wraps every 4.69 s, a 32-bit HPET at 14.3 MHz
        // every 300 s.
        assert_eq!(quarter_wrap_ns(24, 3_579_545), 1_171_742_218);
        assert_eq!(quarter_wrap_ns(32, 14_318_180) / 1_000_000_000, 74);
        assert_eq!(ticks_to_ns(1, 1_000_000), 1000);
        assert_eq!(ticks_to_ns(u64::MAX, 1), i64::MAX);
    }

    #[test]
    fn extends_wrapping_counters() {
        let e = Extender::new(24);
        assert_eq!(e.extend(0xff_fff0), 0xff_fff0);
        assert_eq!(e.extend(0x10), 0x100_0010);
        assert_eq!(e.extend(0x20), 0x100_0020);
        assert_eq!(e.extend(0xff_ffff), 0x1ff_ffff);
        assert_eq!(e.extend(0), 0x200_0000);
        let wide = Extender::new(64);
        assert_eq!(wide.extend(5), 5);
    }
}
