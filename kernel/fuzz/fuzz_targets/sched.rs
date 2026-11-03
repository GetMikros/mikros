//! Runs the run queue as the kernel does: threads are woken, picked, run
//! for their slice or block part-way through it, change priority while
//! queued, and are removed. Compares the queue with the set of queued
//! threads, and checks that no queued thread waits more than three times
//! the longest slice of other threads' run time, however often threads
//! blocked before.

#![no_main]

use kernel_core::sched::{Entity, Fair, MAX_PRIORITY, SLICE_MAX, weight};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

/// Few threads, so that the scheduling period stays at `SLICE_MAX`.
const THREADS: usize = 8;

#[derive(Arbitrary, Debug)]
enum Op {
    Wake(u8),
    /// Runs the next thread for its whole slice, or blocks it after
    /// `ns` when `block` is set.
    Run { ns: u32, block: bool },
    /// Runs the next thread for `ns` and blocks it, `count` times.
    ShortRuns { ns: u16, count: u16 },
    Reweigh { id: u8, priority: u8 },
    Remove(u8),
    Charge(u32),
}

struct Thread {
    entity: Entity,
    queued: bool,
    /// The run time of other threads since this one was queued.
    waited: i64,
}

struct Model {
    fair: Fair<u8>,
    threads: Vec<Thread>,
}

impl Model {
    fn run(&mut self, ns: i64, block: bool) {
        let Some((id, entity)) = self.fair.pick() else {
            assert!(self.threads.iter().all(|t| !t.queued));
            return;
        };
        let t = usize::from(id);
        assert!(self.threads[t].queued);
        self.threads[t].queued = false;
        let slice = self.fair.slice(entity.weight);
        let ran = if block { ns.clamp(0, slice) } else { slice };
        self.fair.charge(ran);
        for other in self.threads.iter_mut().filter(|o| o.queued) {
            other.waited += ran;
            assert!(
                other.waited <= 3 * SLICE_MAX,
                "a queued thread waited {} ns",
                other.waited
            );
        }
        self.threads[t].entity = entity;
        if !block {
            self.wake(t);
        }
    }

    fn wake(&mut self, t: usize) {
        let thread = &mut self.threads[t];
        if !thread.queued {
            thread.queued = true;
            thread.waited = 0;
            self.fair.enqueue(t as u8, thread.entity);
        }
    }
}

fuzz_target!(|ops: Vec<Op>| {
    let mut m = Model {
        fair: Fair::default(),
        threads: (0..THREADS)
            .map(|_| Thread {
                entity: Entity::new(16),
                queued: false,
                waited: 0,
            })
            .collect(),
    };
    for op in ops.iter().take(1024) {
        match *op {
            Op::Wake(id) => m.wake(usize::from(id) % THREADS),
            Op::Run { ns, block } => m.run(i64::from(ns), block),
            Op::ShortRuns { ns, count } => {
                for _ in 0..count % 2048 {
                    m.run(i64::from(ns), true);
                    if let Some(t) = m.threads.iter().position(|t| !t.queued) {
                        m.wake(t);
                    }
                }
            }
            Op::Reweigh { id, priority } => {
                let t = usize::from(id) % THREADS;
                let w = weight(u32::from(priority) % (MAX_PRIORITY + 1));
                assert_eq!(m.fair.reweigh(t as u8, w), m.threads[t].queued);
                if !m.threads[t].queued {
                    m.threads[t].entity.weight = w;
                }
            }
            Op::Remove(id) => {
                let t = usize::from(id) % THREADS;
                let removed = m.fair.remove(t as u8);
                assert_eq!(removed.is_some(), m.threads[t].queued);
                if let Some(entity) = removed {
                    m.threads[t].entity = entity;
                    m.threads[t].queued = false;
                }
            }
            // Time run by threads outside the queue, such as deadline
            // threads, moves virtual time on.
            Op::Charge(ns) => m.fair.charge(i64::from(ns)),
        }
        assert_eq!(m.fair.len(), m.threads.iter().filter(|t| t.queued).count());
    }
});
