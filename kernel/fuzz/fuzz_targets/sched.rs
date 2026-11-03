//! Runs the run queue as the kernel does: threads are woken, picked, run
//! for their slice or block part-way through it, change priority while
//! queued, and are removed. Keeps its own account of the virtual time and
//! of every thread's lag, worked out in floating point from the rules the
//! queue follows, and checks that the queue's lags match it, that no
//! runnable thread's lag leaves ±10 ms, and that no queued thread waits
//! longer than `wait_limit` gives for the threads runnable when it was
//! queued.

#![no_main]

use kernel_core::sched::{Entity, Fair, MAX_PRIORITY, wait_limit, weight};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

const THREADS: usize = 8;
/// The bound on lags.
const LAG_MAX: f64 = 10_000_000.0;
/// How far the queue's integer lags may be from the model's.
const ROUNDING: f64 = 1_000.0;

#[derive(Arbitrary, Debug)]
enum Op {
    Wake(u8),
    /// Runs the next thread for its whole slice, or blocks it after
    /// `ns` when `block` is set.
    Run { ns: u32, block: bool },
    /// Runs the next thread for `ns` and blocks it, then wakes the first
    /// thread that is not runnable, `count` times.
    ShortRuns { ns: u16, count: u16 },
    Reweigh { id: u8, priority: u8 },
    Remove(u8),
}

struct Thread {
    entity: Entity,
    weight: f64,
    runnable: bool,
    /// The time it should have had: its starting lag, and the virtual
    /// time when it started, and the time it had since.
    start_lag: f64,
    start_vtime: f64,
    received: f64,
    /// The run time of other threads since it was queued, and the most
    /// it may be.
    waited: i64,
    limit: i64,
    /// Whether it ever ran.
    ran_once: bool,
}

struct Model {
    fair: Fair<u8>,
    threads: Vec<Thread>,
    vtime: f64,
}

impl Model {
    fn runnable_weight(&self) -> f64 {
        self.threads.iter().filter(|t| t.runnable).map(|t| t.weight).sum()
    }

    fn lag(&self, t: usize) -> f64 {
        let th = &self.threads[t];
        th.start_lag + th.weight * (self.vtime - th.start_vtime) - th.received
    }

    /// Makes thread `t` runnable with lag `lag`.
    fn start(&mut self, t: usize, lag: f64) {
        let th = &mut self.threads[t];
        th.runnable = true;
        th.start_lag = lag;
        th.start_vtime = self.vtime;
        th.received = 0.0;
        let total = self.runnable_weight();
        self.vtime -= lag / total;
    }

    /// Makes runnable thread `t` stop, and returns its lag.
    fn stop(&mut self, t: usize) -> f64 {
        let lag = self.lag(t);
        self.threads[t].runnable = false;
        let rest = self.runnable_weight();
        if rest > 0.0 {
            self.vtime += lag / rest;
        }
        lag
    }

    fn wake(&mut self, t: usize) {
        if self.threads[t].runnable {
            return;
        }
        let e = self.threads[t].entity;
        let saved = e.saved_lag() as f64;
        let lag = if self.threads[t].ever_ran() && saved <= 0.0 { saved } else { 0.0 };
        self.start(t, lag);
        self.fair.enqueue(t as u8, e);
        let n = self.threads.iter().filter(|t| t.runnable).count();
        self.threads[t].waited = 0;
        self.threads[t].limit = wait_limit(n);
    }

    fn run(&mut self, ns: i64, block: bool) {
        let Some((id, _)) = self.fair.pick() else {
            assert!(self.threads.iter().all(|t| !t.runnable));
            return;
        };
        let t = usize::from(id);
        assert!(self.threads[t].runnable);
        let slice = self.fair.slice();
        let ran = if block { ns.clamp(0, slice) } else { slice };
        self.fair.charge(ran);
        let total = self.runnable_weight();
        self.vtime += ran as f64 / total;
        self.threads[t].received += ran as f64;
        self.threads[t].ran_once = true;
        for (i, other) in self.threads.iter_mut().enumerate() {
            if other.runnable && i != t {
                other.waited += ran;
                // Threads whose deadlines come together start 1 ns apart.
                assert!(
                    other.waited <= other.limit + THREADS as i64,
                    "thread {i} waited {} ns of {}",
                    other.waited,
                    other.limit
                );
            }
        }
        self.threads[t].waited = 0;
        if block {
            self.stop(t);
            self.threads[t].entity = self.fair.stop().expect("the running thread stops");
        } else {
            self.fair.requeue(id);
            let n = self.threads.iter().filter(|t| t.runnable).count();
            self.threads[t].limit = wait_limit(n);
        }
    }

    fn check(&self) {
        for (t, th) in self.threads.iter().enumerate() {
            let queued = self.fair.lag(t as u8);
            assert_eq!(queued.is_some(), th.runnable, "thread {t}");
            let Some(queued) = queued else { continue };
            let lag = self.lag(t);
            assert!(
                (queued as f64 - lag).abs() <= ROUNDING,
                "thread {t}: lag {queued} where {lag} was expected"
            );
            assert!(lag.abs() <= LAG_MAX, "thread {t} has lag {lag}");
        }
    }
}

impl Thread {
    fn ever_ran(&self) -> bool {
        self.ran_once
    }
}

fuzz_target!(|ops: Vec<Op>| {
    let mut m = Model {
        fair: Fair::default(),
        threads: (0..THREADS)
            .map(|_| Thread {
                entity: Entity::new(16),
                weight: weight(16) as f64,
                runnable: false,
                start_lag: 0.0,
                start_vtime: 0.0,
                received: 0.0,
                waited: 0,
                limit: 0,
                ran_once: false,
            })
            .collect(),
        vtime: 0.0,
    };
    for op in ops.iter().take(1024) {
        match *op {
            Op::Wake(id) => m.wake(usize::from(id) % THREADS),
            Op::Run { ns, block } => m.run(i64::from(ns), block),
            Op::ShortRuns { ns, count } => {
                for _ in 0..count % 2048 {
                    m.run(i64::from(ns), true);
                    if let Some(t) = m.threads.iter().position(|t| !t.runnable) {
                        m.wake(t);
                    }
                }
            }
            Op::Reweigh { id, priority } => {
                let t = usize::from(id) % THREADS;
                let w = weight(u32::from(priority) % (MAX_PRIORITY + 1));
                assert_eq!(m.fair.reweigh(t as u8, w), m.threads[t].runnable);
                if m.threads[t].runnable {
                    let lag = m.stop(t);
                    m.threads[t].weight = w as f64;
                    m.start(t, lag);
                } else {
                    m.threads[t].entity.weight = w;
                    m.threads[t].weight = w as f64;
                }
            }
            Op::Remove(id) => {
                let t = usize::from(id) % THREADS;
                let removed = m.fair.remove(t as u8);
                assert_eq!(removed.is_some(), m.threads[t].runnable);
                if let Some(entity) = removed {
                    m.stop(t);
                    m.threads[t].entity = entity;
                }
            }
        }
        m.check();
    }
});
