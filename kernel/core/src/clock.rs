//! Turning hardware counter values into the monotonic clock.

use core::sync::atomic::{AtomicU64, Ordering};

/// Nanoseconds for `ticks` of a counter running at `hz`.
pub fn ticks_to_ns(ticks: u64, hz: u64) -> i64 {
    let ns = u128::from(ticks) * 1_000_000_000 / u128::from(hz.max(1));
    i64::try_from(ns).unwrap_or(i64::MAX)
}

/// Extends a counter of `bits` bits that wraps around to 64 bits. A value
/// is only extended correctly when it was read less than half the wrap
/// period after the newest value extended so far, or before it: a reader
/// on another processor may extend a later reading between this reader's
/// reading and its extension, and that is not a wrap.
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
        let half = 1u64 << (self.bits - 1);
        let raw = raw & mask;
        let mut last = self.last.load(Ordering::Relaxed);
        loop {
            let ahead = raw.wrapping_sub(last) & mask;
            // A reading from before the newest extended value gets the
            // value it had then, which leaves the newest one as it is. Only
            // the first reading of all may be anywhere.
            if ahead >= half && last != 0 {
                return last.saturating_sub(last.wrapping_sub(raw) & mask);
            }
            let value = last + ahead;
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
        assert_eq!(e.extend(0x7f_ffff), 0x17f_ffff);
        assert_eq!(e.extend(0xff_fff0), 0x1ff_fff0);
        assert_eq!(e.extend(0), 0x200_0000);
        let wide = Extender::new(64);
        assert_eq!(wide.extend(5), 5);
    }

    #[test]
    fn does_not_take_an_earlier_reading_for_a_wrap() {
        // A reader read 0x1000 and another read 0x1010 and extended it
        // first: the first one's reading is from before, not a wrap later.
        let e = Extender::new(32);
        assert_eq!(e.extend(0x1010), 0x1010);
        assert_eq!(e.extend(0x1000), 0x1000);
        assert_eq!(e.extend(0x1020), 0x1020);
        // Also across a wrap, either way.
        let e = Extender::new(24);
        assert_eq!(e.extend(0xff_fff0), 0xff_fff0);
        assert_eq!(e.extend(0x10), 0x100_0010);
        assert_eq!(e.extend(0xff_fff8), 0xff_fff8);
        assert_eq!(e.extend(0x20), 0x100_0020);
        let e = Extender::new(24);
        assert_eq!(e.extend(0x10), 0x10);
        assert_eq!(e.extend(0x08), 0x08);
    }

    #[test]
    fn keeps_concurrent_readers_within_one_wrap() {
        // Readers on several threads read a shared 16-bit counter that
        // each also advances, and extend their readings in whatever order
        // they come; none may get a value a wrap ahead of the counter.
        use core::sync::atomic::AtomicU64;
        use std::sync::Arc;
        let counter = Arc::new(AtomicU64::new(0));
        let e = Arc::new(Extender::new(16));
        let threads: std::vec::Vec<_> = (0..4)
            .map(|_| {
                let (counter, e) = (counter.clone(), e.clone());
                std::thread::spawn(move || {
                    for _ in 0..100_000 {
                        let reading = counter.fetch_add(1, Ordering::Relaxed);
                        let value = e.extend(reading & 0xffff);
                        assert!(value <= counter.load(Ordering::Relaxed));
                        assert!(value + 0x8000 > reading);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
