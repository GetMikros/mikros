//! Runs run queue operations and compares the queue with the set of
//! queued threads.

#![no_main]

use kernel_core::sched::{Entity, Fair, MAX_PRIORITY};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;

#[derive(Arbitrary, Debug)]
enum Op {
    Enqueue { id: u8, priority: u8 },
    Remove(u8),
    Pick,
    Charge(u32),
}

fuzz_target!(|ops: Vec<Op>| {
    let mut fair: Fair<u8> = Fair::default();
    let mut queued: BTreeSet<u8> = BTreeSet::new();
    for op in ops.iter().take(1024) {
        match *op {
            Op::Enqueue { id, priority } => {
                // Only threads that are not queued are enqueued.
                if queued.insert(id) {
                    let priority = u32::from(priority) % (MAX_PRIORITY + 1);
                    fair.enqueue(id, Entity::new(priority));
                }
            }
            Op::Remove(id) => assert_eq!(fair.remove(id).is_some(), queued.remove(&id)),
            Op::Pick => match fair.pick() {
                Some((id, _)) => assert!(queued.remove(&id)),
                None => assert!(queued.is_empty()),
            },
            Op::Charge(ns) => fair.charge(i64::from(ns)),
        }
        assert_eq!(fair.len(), queued.len());
    }
});
