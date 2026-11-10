//! Extends a wrapping counter of any width read at intervals shorter
//! than half its wrap period, as the clock does, and readings made before
//! the newest one was extended, as other processors do, and compares each
//! value with the counter's true 64-bit value. Also compares tick
//! conversions with a model.

#![no_main]

use kernel_core::clock::{Extender, quarter_wrap_ns, ticks_to_ns};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    bits: u8,
    /// How far the counter moves before each read, below half its wrap
    /// period.
    steps: Vec<u64>,
    /// After each read, how far behind it an earlier reading is that is
    /// extended only then, below half the wrap period.
    behind: Vec<u64>,
    /// Set in each read above the counter's width, which must be ignored.
    noise: u64,
    hz: u64,
    ticks: u64,
}

fuzz_target!(|input: Input| {
    let bits = u32::from(input.bits) % 64 + 1;
    let period = 1u128 << bits;
    let e = Extender::new(bits);
    let mask = (period - 1) as u64;
    let half = if bits == 64 { u64::MAX } else { (period / 2) as u64 };
    let raw = |at: u64| if bits == 64 { at } else { at | input.noise & !mask };
    let mut counter = 0u64;
    let mut last = 0;
    for (i, &step) in input.steps.iter().take(512).enumerate() {
        let step = step % half.max(1);
        let Some(next) = counter.checked_add(step) else { break };
        counter = next;
        let value = e.extend(raw(counter));
        assert_eq!(value, counter);
        assert!(value >= last);
        last = value;
        let back = input.behind.get(i).map_or(0, |&b| b % half.max(1));
        if let Some(earlier) = counter.checked_sub(back) {
            assert_eq!(e.extend(raw(earlier)), earlier);
        }
    }
    let ns = u128::from(input.ticks) * 1_000_000_000 / u128::from(input.hz.max(1));
    assert_eq!(ticks_to_ns(input.ticks, input.hz), i64::try_from(ns).unwrap_or(i64::MAX));
    if bits < 64 {
        let quarter = (period * 1_000_000_000 / u128::from(input.hz.max(1))) / 4;
        assert_eq!(quarter_wrap_ns(bits, input.hz), i64::try_from(quarter).unwrap_or(i64::MAX));
    }
});
