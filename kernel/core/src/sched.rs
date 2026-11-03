//! The weighted fair scheduling of ordinary threads on one processor.
//!
//! In each scheduling period every runnable thread gets its weight's
//! share of the period: one slice when the share is at most `SLICE_MAX`,
//! otherwise equal slices of at most `SLICE_MAX`. Virtual time advances
//! by `ROUND` for each period of run time. A queued thread's slice starts
//! at the later of its own position and the virtual time, and its
//! position moves to the end of the slice, its part of `ROUND`. Threads
//! run in order of position, in queuing order when positions are equal,
//! but a thread whose slice has not started yet waits while another can
//! run. So a thread with one slice comes back about one period later, and
//! a thread with several comes back after the matching part of a period,
//! without drifting from its share.

use alloc::collections::{TryReserveError, VecDeque};

/// The virtual time of one whole share.
const ROUND: u64 = 1 << 40;

/// The weights of priorities 0 to 32: `round(1024 × 1.25^(p − 16))`.
const WEIGHTS: [u64; 33] = [
    29, 36, 45, 56, 70, 88, 110, 137, 172, 215, 268, 336, 419, 524, 655, 819, 1024, 1280, 1600,
    2000, 2500, 3125, 3906, 4883, 6104, 7629, 9537, 11921, 14901, 18626, 23283, 29104, 36380,
];

/// The priority of a new thread.
pub const DEFAULT_PRIORITY: u32 = 16;
/// The highest priority a thread can be given.
pub const MAX_PRIORITY: u32 = 31;

/// The longest slice, and the shortest scheduling period.
pub const SLICE_MAX: i64 = 10_000_000;
/// The scheduling period grows by this much per runnable thread.
pub const PERIOD_PER_THREAD: i64 = 1_000_000;

/// The weight of priority `priority`, which is at most 32.
pub fn weight(priority: u32) -> u64 {
    WEIGHTS[priority.min(32) as usize]
}

/// A thread's slice and how many slices its share is split into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Share {
    slice: i64,
    pieces: usize,
}

/// A thread's scheduling state, kept while it is not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entity {
    pub weight: u64,
    /// The virtual time at which its next slice may start.
    start: u64,
    /// The virtual time at which its next slice ends.
    position: u64,
    /// The remainder of splitting `ROUND` into slices.
    carry: u64,
}

impl Entity {
    /// The state of a new thread with `priority`.
    pub fn new(priority: u32) -> Entity {
        Entity {
            weight: weight(priority),
            start: 0,
            position: 0,
            carry: 0,
        }
    }
}

/// Whether position `a` is before `b`, allowing for wrap-around.
fn before(a: u64, b: u64) -> bool {
    (a.wrapping_sub(b) as i64) < 0
}

#[derive(Clone, Copy)]
struct Queued<T> {
    id: T,
    entity: Entity,
}

/// The runnable ordinary threads of one processor, other than the one
/// running. Queuing a thread does not allocate as long as room was
/// reserved for it.
pub struct Fair<T: Copy + PartialEq> {
    /// Sorted by position, in queuing order among equal positions.
    queue: VecDeque<Queued<T>>,
    /// The total weight of the queued threads.
    total: u64,
    /// The virtual time.
    now: u64,
}

impl<T: Copy + PartialEq> Default for Fair<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq> Fair<T> {
    /// The size of the entry of one queued thread.
    pub const ENTRY_SIZE: usize = size_of::<Queued<T>>();

    pub const fn new() -> Self {
        Fair {
            queue: VecDeque::new(),
            total: 0,
            now: 0,
        }
    }

    /// Makes room for `additional` more queued threads.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.queue.try_reserve(additional)
    }

    /// The number of queued threads.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The share of a thread of `weight` that is not queued, among the
    /// queued threads.
    fn share(&self, weight: u64) -> Share {
        let n = self.queue.len() as i64 + 1;
        let period = SLICE_MAX.max(n * PERIOD_PER_THREAD);
        let total = self.total + weight;
        let share = (i128::from(period) * i128::from(weight) / i128::from(total.max(1))) as i64;
        let pieces = ((share + SLICE_MAX - 1) / SLICE_MAX).max(1);
        Share {
            slice: (share / pieces).max(1),
            pieces: pieces as usize,
        }
    }

    /// How long a thread of `weight` that is not queued may run before
    /// the next thread's turn.
    pub fn slice(&self, weight: u64) -> i64 {
        self.share(weight).slice
    }

    /// Queues a thread that is new, was blocked, or used up its slice.
    pub fn enqueue(&mut self, id: T, mut entity: Entity) {
        let pieces = self.share(entity.weight).pieces as u64;
        let step = ROUND + entity.carry;
        entity.carry = step % pieces;
        // A thread that ran little before it blocked has moved its
        // position on by whole steps; its slice starts no later than one
        // round ahead, so that it waits no more than about one period.
        let latest = self.now.wrapping_add(ROUND);
        entity.start = if before(entity.position, self.now) {
            self.now
        } else if before(latest, entity.position) {
            latest
        } else {
            entity.position
        };
        entity.position = entity.start.wrapping_add(step / pieces);
        let at = self
            .queue
            .partition_point(|q| !before(entity.position, q.entity.position));
        self.queue.insert(at, Queued { id, entity });
        self.total += entity.weight;
    }

    /// Removes `id` from the queue, returning its state.
    pub fn remove(&mut self, id: T) -> Option<Entity> {
        let i = self.queue.iter().position(|q| q.id == id)?;
        let q = self.queue.remove(i)?;
        self.total -= q.entity.weight;
        Some(q.entity)
    }

    /// Gives queued thread `id` `weight`, keeping its place, and returns
    /// whether it is queued.
    pub fn reweigh(&mut self, id: T, weight: u64) -> bool {
        let Some(q) = self.queue.iter_mut().find(|q| q.id == id) else {
            return false;
        };
        self.total = self.total - q.entity.weight + weight;
        q.entity.weight = weight;
        true
    }

    /// Takes the thread to run next, with its state: the first in order
    /// whose slice may start, or the first if none may.
    pub fn pick(&mut self) -> Option<(T, Entity)> {
        let i = self
            .queue
            .iter()
            .position(|q| !before(self.now, q.entity.start))
            .unwrap_or(0);
        let q = self.queue.remove(i)?;
        self.total -= q.entity.weight;
        Some((q.id, q.entity))
    }

    /// Advances virtual time for `ran` nanoseconds that the running
    /// thread ran.
    pub fn charge(&mut self, ran: i64) {
        let n = self.queue.len() as i64 + 1;
        let period = SLICE_MAX.max(n * PERIOD_PER_THREAD);
        let step = u128::from(ran.max(0) as u64) * u128::from(ROUND) / period as u128;
        self.now = self.now.wrapping_add(step as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    #[test]
    fn weights_follow_the_formula() {
        for p in 0..=32 {
            let expected = (1024.0 * 1.25f64.powi(p as i32 - 16)).round() as u64;
            assert_eq!(weight(p), expected, "priority {p}");
        }
    }

    /// Runs `priorities` for `duration` ns, all always runnable, and
    /// checks each thread's time against its share and the gaps between
    /// its runs.
    fn simulate(priorities: &[u32], duration: i64) {
        let n = priorities.len();
        let mut fair = Fair::new();
        let mut ran = vec![0i64; n];
        let mut last_end = vec![0i64; n];
        let mut max_gap = vec![0i64; n];
        for (i, &p) in priorities.iter().enumerate() {
            fair.enqueue(i, Entity::new(p));
        }
        let total: u64 = priorities.iter().map(|&p| weight(p)).sum();
        let interval = SLICE_MAX.max(n as i64 * PERIOD_PER_THREAD);
        let mut now = 0i64;
        while now < duration {
            let (id, e) = fair.pick().unwrap();
            let slice = fair.slice(e.weight);
            max_gap[id] = max_gap[id].max(now - last_end[id]);
            now += slice;
            ran[id] += slice;
            last_end[id] = now;
            fair.charge(slice);
            fair.enqueue(id, e);
            for (j, &p) in priorities.iter().enumerate() {
                let share = (i128::from(now) * i128::from(weight(p)) / i128::from(total)) as i64;
                assert!(
                    (ran[j] - share).abs() <= SLICE_MAX,
                    "thread {j} at {now}: ran {} for a share of {share}",
                    ran[j]
                );
            }
        }
        for (j, &gap) in max_gap.iter().enumerate() {
            assert!(
                gap <= interval,
                "thread {j} waited {gap} ns, more than {interval}"
            );
        }
    }

    #[test]
    fn shares_by_weight() {
        simulate(&[16, 16, 16], 1_000_000_000);
        simulate(&[0, 16, 31], 1_000_000_000);
        simulate(&[31, 31, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 1_000_000_000);
        simulate(&[16], 100_000_000);
    }

    #[test]
    fn spreads_large_shares_over_the_period() {
        let mut many = vec![16u32; 99];
        many.push(31);
        simulate(&many, 2_000_000_000);
        let mut two = vec![0u32; 40];
        two.extend([31, 30]);
        simulate(&two, 2_000_000_000);
    }

    #[test]
    fn holds_for_many_mixes_of_priorities() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..40 {
            let n = 1 + (next() % 150) as usize;
            let priorities: std::vec::Vec<u32> = (0..n).map(|_| (next() % 32) as u32).collect();
            simulate(&priorities, 500_000_000);
        }
    }

    #[test]
    fn gives_no_credit_for_blocked_time() {
        let mut fair = Fair::new();
        fair.enqueue(1, Entity::new(16));
        fair.enqueue(2, Entity::new(16));
        let (_, sleeper) = fair.pick().unwrap();
        // Thread 2 runs alone for a second while thread 1 is blocked.
        let (id, mut runner) = fair.pick().unwrap();
        assert_eq!(id, 2);
        for _ in 0..100 {
            let slice = fair.slice(runner.weight);
            fair.charge(slice);
            fair.enqueue(2, runner);
            runner = fair.pick().unwrap().1;
        }
        fair.enqueue(2, runner);
        fair.enqueue(1, sleeper);
        // The sleeper gets one slice, not a second's worth, and then they
        // alternate.
        let order: std::vec::Vec<u32> = (0..4)
            .map(|_| {
                let (id, e) = fair.pick().unwrap();
                fair.charge(fair.slice(e.weight));
                fair.enqueue(id, e);
                id
            })
            .collect();
        assert_eq!(order, [1, 2, 1, 2]);
    }

    /// Runs thread 1 alone for `wakes` runs of 10 us that each end in a
    /// block, then queues the always runnable thread 2 and wakes thread
    /// 1, and returns how long thread 2 runs before thread 1 does.
    fn wait_after_short_runs(wakes: usize, reweigh: bool) -> i64 {
        let mut fair = Fair::new();
        let mut short = Entity::new(16);
        for _ in 0..wakes {
            fair.enqueue(1, short);
            if reweigh {
                fair.reweigh(1, weight(17));
                fair.reweigh(1, weight(16));
            }
            let (_, e) = fair.pick().unwrap();
            fair.charge(10_000);
            short = e;
        }
        fair.enqueue(2, Entity::new(16));
        fair.enqueue(1, short);
        let mut waited = 0;
        loop {
            let (id, e) = fair.pick().unwrap();
            if id == 1 {
                return waited;
            }
            let slice = fair.slice(e.weight);
            fair.charge(slice);
            waited += slice;
            fair.enqueue(id, e);
        }
    }

    #[test]
    fn bounds_the_wait_after_many_short_runs() {
        for wakes in [0, 1, 10, 1000] {
            let waited = wait_after_short_runs(wakes, false);
            assert!(waited <= SLICE_MAX, "after {wakes} runs: {waited} ns");
        }
        let waited = wait_after_short_runs(1000, true);
        assert!(waited <= SLICE_MAX, "after reweighing: {waited} ns");
    }

    #[test]
    fn queues_threads_in_order() {
        let mut fair = Fair::new();
        fair.enqueue(1, Entity::new(16));
        fair.enqueue(2, Entity::new(16));
        fair.enqueue(3, Entity::new(31));
        assert_eq!(fair.pick().map(|(id, e)| (id, e.weight)), Some((1, 1024)));
        assert_eq!(fair.slice(1024), SLICE_MAX * 1024 / (2048 + 29104));
        assert!(fair.remove(3).is_some());
        assert!(fair.remove(3).is_none());
        assert_eq!(fair.slice(1024), SLICE_MAX / 2);
        assert_eq!(Fair::<u32>::new().slice(1024), SLICE_MAX);
    }
}
