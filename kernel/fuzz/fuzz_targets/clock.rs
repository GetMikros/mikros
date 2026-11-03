//! Extends a wrapping counter of any width read at intervals shorter
//! than its wrap period, as the clock does, and compares each value with
//! the counter's true 64-bit value. Also compares tick conversions with
//! a model.

#![no_main]

use kernel_core::clock::{Extender, quarter_wrap_ns, ticks_to_ns};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct Input {
    bits: u8,
    /// How far the counter moves before each read, below its wrap period.
    steps: Vec<u64>,
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
    let mut counter = 0u64;
    let mut last = 0;
    for &step in input.steps.iter().take(512) {
        let step = (u128::from(step) % period) as u64;
        let Some(next) = counter.checked_add(step) else { break };
        counter = next;
        let raw = if bits == 64 { counter } else { counter | input.noise & !mask };
        let value = e.extend(raw);
        assert_eq!(value, counter);
        assert!(value >= last);
        last = value;
    }
    let ns = u128::from(input.ticks) * 1_000_000_000 / u128::from(input.hz.max(1));
    assert_eq!(ticks_to_ns(input.ticks, input.hz), i64::try_from(ns).unwrap_or(i64::MAX));
    if bits < 64 {
        let quarter = (period * 1_000_000_000 / u128::from(input.hz.max(1))) / 4;
        assert_eq!(quarter_wrap_ns(bits, input.hz), i64::try_from(quarter).unwrap_or(i64::MAX));
    }
});
