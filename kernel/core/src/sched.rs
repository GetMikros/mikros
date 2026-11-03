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
//!
//! A queued thread also gets a deadline, in run time: the period it is
//! queued for, as `wait_limit` gives it. No slice runs past the earliest
//! deadline of the queued threads, and a thread whose deadline has come
//! runs before the others; threads whose deadlines come together start
//! up to 1 ns apart, as every slice lasts at least 1 ns. Slices are sized
//! when their threads are
//! picked, so without this a queue that shrank or changed weights could
//! add up a longer wait than one period. A slice cut short moves its
//! thread's position on only by the part it got, so it keeps its share.

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
    /// The run time by which it must have started to run.
    deadline: i64,
}

/// The longest a thread queued among `n` queued threads, itself
/// included, may wait for its next run.
pub fn wait_limit(n: usize) -> i64 {
    SLICE_MAX.max(n as i64 * PERIOD_PER_THREAD)
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
    /// The run time charged so far.
    elapsed: i64,
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
            elapsed: 0,
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
        self.share(weight).slice.min(self.until_due())
    }

    /// The run time left until the earliest deadline of the queued
    /// threads, at least 1.
    fn until_due(&self) -> i64 {
        self.queue
            .iter()
            .map(|q| (q.deadline - self.elapsed).max(1))
            .min()
            .unwrap_or(i64::MAX)
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
        let deadline = self.elapsed + wait_limit(self.queue.len() + 1);
        self.queue.insert(
            at,
            Queued {
                id,
                entity,
                deadline,
            },
        );
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

    /// Takes the thread to run next, with its state: the one whose
    /// deadline came first if any has come, otherwise the first in order
    /// whose slice may start, or the first if none may. When `slice` will
    /// cut its slice short for the next deadline, its position moves on
    /// only by the part of the slice it gets.
    pub fn pick(&mut self) -> Option<(T, Entity)> {
        let due = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, q)| q.deadline <= self.elapsed)
            .min_by_key(|(_, q)| q.deadline)
            .map(|(i, _)| i);
        let i = due.unwrap_or_else(|| {
            self.queue
                .iter()
                .position(|q| !before(self.now, q.entity.start))
                .unwrap_or(0)
        });
        let q = self.queue.remove(i)?;
        self.total -= q.entity.weight;
        let mut entity = q.entity;
        let full = self.share(entity.weight).slice;
        let slice = full.min(self.until_due());
        if slice < full {
            let step = entity.position.wrapping_sub(entity.start);
            let part = u128::from(step) * slice as u128 / full as u128;
            entity.position = entity.start.wrapping_add(part as u64);
        }
        Some((q.id, entity))
    }

    /// Advances virtual time for `ran` nanoseconds that the running
    /// thread ran.
    pub fn charge(&mut self, ran: i64) {
        let n = self.queue.len() as i64 + 1;
        let period = SLICE_MAX.max(n * PERIOD_PER_THREAD);
        let step = u128::from(ran.max(0) as u64) * u128::from(ROUND) / period as u128;
        self.now = self.now.wrapping_add(step as u64);
        self.elapsed += ran.max(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_follow_the_formula() {
        for p in 0..=32 {
            let expected = (1024.0 * 1.25f64.powi(p as i32 - 16)).round() as u64;
            assert_eq!(weight(p), expected, "priority {p}");
        }
    }
}
