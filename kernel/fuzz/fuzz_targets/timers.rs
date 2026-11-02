//! Runs deadline queue operations and compares the queue with a list of
//! waiting items.

#![no_main]

use kernel_core::deadline::{Deadlines, INFINITE};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
enum Op {
    Insert { deadline: i16, infinite: bool },
    PopExpired(i16),
    Remove(u16),
}

fuzz_target!(|ops: Vec<Op>| {
    let mut deadlines: Deadlines<u16> = Deadlines::default();
    // The waiting items with their deadlines, in insertion order.
    let mut model: Vec<(i64, u16)> = Vec::new();
    for (n, op) in ops.iter().take(1024).enumerate() {
        match *op {
            Op::Insert { deadline, infinite } => {
                let deadline = if infinite { INFINITE } else { i64::from(deadline) };
                deadlines.insert(deadline, n as u16).unwrap();
                model.push((deadline, n as u16));
            }
            Op::PopExpired(now) => {
                let now = i64::from(now);
                // The earliest expired item, the first inserted among
                // equal deadlines.
                let expected = model
                    .iter()
                    .enumerate()
                    .filter(|&(_, &(d, _))| d != INFINITE && d <= now)
                    .min_by_key(|&(i, &(d, _))| (d, i))
                    .map(|(i, _)| i);
                let popped = deadlines.pop_expired(now);
                assert_eq!(popped, expected.map(|i| model.remove(i).1));
            }
            Op::Remove(item) => {
                let at = model.iter().position(|&(_, i)| i == item);
                assert_eq!(deadlines.remove(&item), at.is_some());
                if let Some(at) = at {
                    model.remove(at);
                }
            }
        }
        assert_eq!(deadlines.len(), model.len());
        assert_eq!(deadlines.next(), model.iter().map(|&(d, _)| d).min());
    }
});
