//! The weighted fair scheduling of ordinary threads on one processor.
//!
//! The processor keeps a virtual time that advances by the run time of
//! these threads divided by the total weight of the runnable ones. Each
//! runnable thread has a virtual point `v`, which advances by its own run
//! time divided by its weight, and its lag, the time it should have had
//! minus the time it had, is its weight times how far `v` is behind the
//! virtual time. A thread that stops takes its lag with it, and the
//! virtual time moves so that the others' lags still add up to zero; a
//! thread that starts again brings its lag back if it was not positive.
//!
//! A thread is eligible while its lag is not negative. It asks for a
//! request, its part of the scheduling period, and its virtual deadline
//! is `v` plus its request over its weight. The eligible thread with the
//! earliest virtual deadline runs next, for its request.
//!
//! A queued thread also gets a deadline in run time, the period it is
//! queued for, as `wait_limit` gives it. No slice runs past the earliest
//! of those deadlines, and a thread whose deadline has come runs before
//! the others, for no more than `MIN_SLICE` when it is not eligible.
//! Threads whose deadlines come together start up to `MIN_SLICE` apart.

use alloc::collections::{TryReserveError, VecDeque};

/// The weights of priorities 0 to 32: `round(1024 × 1.25^(p − 16))`.
const WEIGHTS: [u64; 33] = [
    29, 36, 45, 56, 70, 88, 110, 137, 172, 215, 268, 336, 419, 524, 655, 819, 1024, 1280, 1600,
    2000, 2500, 3125, 3906, 4883, 6104, 7629, 9537, 11921, 14901, 18626, 23283, 29104, 36380,
];

/// The priority of a new thread.
pub const DEFAULT_PRIORITY: u32 = 16;
/// The highest priority a thread can be given.
pub const MAX_PRIORITY: u32 = 31;

/// The shortest scheduling period, and the longest wait.
pub const SLICE_MAX: i64 = 10_000_000;
/// The scheduling period grows by this much per runnable thread.
pub const PERIOD_PER_THREAD: i64 = 1_000_000;
/// The longest request, which bounds how far a thread's lag moves in
/// one run.
pub const REQUEST_MAX: i64 = 4_000_000;
/// The shortest request.
pub const MIN_SLICE: i64 = 50_000;

/// The fraction bits of virtual times.
const SHIFT: u32 = 32;

/// The weight of priority `priority`, which is at most 32.
pub fn weight(priority: u32) -> u64 {
    WEIGHTS[priority.min(32) as usize]
}

/// The longest a thread queued among `n` runnable threads, itself
/// included, may wait for its next run.
pub fn wait_limit(n: usize) -> i64 {
    SLICE_MAX.max(n as i64 * PERIOD_PER_THREAD)
}

/// `time` in virtual time units for `weight`.
fn virt(time: i64, weight: u64) -> i128 {
    (i128::from(time) << SHIFT) / i128::from(weight.max(1))
}

/// A thread's scheduling state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entity {
    pub weight: u64,
    /// Its virtual point while it is runnable.
    v: i128,
    /// Its virtual deadline while it is runnable.
    vd: i128,
    /// What it asked for its current run.
    request: i64,
    /// Its lag when it last stopped, in nanoseconds with `SHIFT`
    /// fraction bits.
    lag: i128,
    /// Whether it was picked to run since it was made.
    ran: bool,
}

impl Entity {
    /// The state of a new thread with `priority`.
    pub fn new(priority: u32) -> Entity {
        Entity {
            weight: weight(priority),
            v: 0,
            vd: 0,
            request: 0,
            lag: 0,
            ran: false,
        }
    }

    /// Its lag when it last stopped, which it starts with again when it
    /// is not positive.
    pub fn saved_lag(&self) -> i64 {
        (self.lag >> SHIFT) as i64
    }
}

#[derive(Clone, Copy)]
struct Queued<T> {
    id: T,
    entity: Entity,
    /// The run time by which it must have started to run.
    deadline: i64,
}

/// The runnable ordinary threads of one processor: the queued ones, and
/// the running one, if any. Queuing a thread does not allocate as long
/// as room was reserved for it.
pub struct Fair<T: Copy + PartialEq> {
    queue: VecDeque<Queued<T>>,
    /// The total weight of the queued threads.
    total: u64,
    /// The running thread, which `pick` took.
    running: Option<(T, Entity)>,
    /// The running thread again, when it was made runnable again before
    /// it stopped, to be queued instead of stopping.
    rewoken: Option<T>,
    /// The virtual time.
    vtime: i128,
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
            running: None,
            rewoken: None,
            vtime: 0,
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

    /// The total weight of the runnable threads.
    fn runnable_weight(&self) -> u64 {
        self.total + self.running.map_or(0, |(_, e)| e.weight)
    }

    /// The number of runnable threads.
    fn runnable(&self) -> usize {
        self.queue.len() + usize::from(self.running.is_some())
    }

    /// The lag of `entity`, which is runnable, with `SHIFT` fraction
    /// bits, so that stopping and starting again loses nothing.
    fn lag_of(&self, entity: &Entity) -> i128 {
        (self.vtime - entity.v) * i128::from(entity.weight)
    }

    /// Moves the virtual time for a thread with lag `lag` stopping, when
    /// `rest` is the weight of the runnable threads left.
    fn on_stop(&mut self, lag: i128, rest: u64) {
        if rest > 0 {
            self.vtime += lag / i128::from(rest);
        }
    }

    /// Makes `entity` runnable with lag `lag`, moving the virtual time
    /// back; `others` is the weight of the other runnable threads.
    fn on_start(&mut self, entity: &mut Entity, lag: i128, others: u64) {
        entity.v = self.vtime - lag / i128::from(entity.weight.max(1));
        self.vtime -= lag / i128::from((others + entity.weight).max(1));
    }

    /// Gives `entity`, about to be queued, its request and virtual
    /// deadline, and queues it under `id`.
    fn insert(&mut self, id: T, mut entity: Entity) {
        let n = self.runnable() + 1;
        let period = SLICE_MAX.max(n as i64 * PERIOD_PER_THREAD);
        let total = self.runnable_weight() + entity.weight;
        let share = (i128::from(period) * i128::from(entity.weight) / i128::from(total)) as i64;
        entity.request = share.clamp(MIN_SLICE, REQUEST_MAX);
        entity.vd = entity.v + virt(entity.request, entity.weight);
        let deadline = self.elapsed + wait_limit(n);
        self.queue.push_back(Queued {
            id,
            entity,
            deadline,
        });
        self.total += entity.weight;
    }

    /// Makes a thread that is new or stopped runnable and queues it. It
    /// starts with the lag it stopped with when that was not positive.
    /// When it is the running thread, which was made runnable again
    /// before it stopped, it is queued when it stops instead.
    pub fn enqueue(&mut self, id: T, entity: Entity) {
        if self.running.is_some_and(|(r, _)| r == id) {
            self.rewoken = Some(id);
            return;
        }
        let mut entity = entity;
        let lag = if entity.ran && entity.lag <= 0 {
            entity.lag
        } else {
            0
        };
        let others = self.runnable_weight();
        self.on_start(&mut entity, lag, others);
        self.insert(id, entity);
    }

    /// Queues the running thread again, under `id`, as it stays runnable.
    pub fn requeue(&mut self, id: T) {
        if let Some((_, entity)) = self.running.take() {
            self.insert(id, entity);
        }
    }

    /// Takes the running thread out, as it stopped being runnable, and
    /// returns its state with its lag kept. When it was made runnable
    /// again meanwhile, it is queued instead and `None` is returned.
    pub fn stop(&mut self) -> Option<Entity> {
        if let Some(id) = self.rewoken.take() {
            self.requeue(id);
            return None;
        }
        let (_, mut entity) = self.running.take()?;
        entity.lag = self.lag_of(&entity);
        let rest = self.runnable_weight();
        self.on_stop(entity.lag, rest);
        Some(entity)
    }

    /// Removes queued thread `id`, as it stopped being runnable, and
    /// returns its state with its lag kept.
    pub fn remove(&mut self, id: T) -> Option<Entity> {
        let i = self.queue.iter().position(|q| q.id == id)?;
        let q = self.queue.remove(i)?;
        self.total -= q.entity.weight;
        let mut entity = q.entity;
        entity.lag = self.lag_of(&entity);
        let rest = self.runnable_weight();
        self.on_stop(entity.lag, rest);
        Some(entity)
    }

    /// Gives queued thread `id` `weight`, as if it stopped and started
    /// again with the same lag, and returns whether it is queued.
    pub fn reweigh(&mut self, id: T, weight: u64) -> bool {
        let Some(i) = self.queue.iter().position(|q| q.id == id) else {
            return false;
        };
        let q = self.queue.remove(i).expect("found above");
        self.total -= q.entity.weight;
        let mut entity = q.entity;
        let lag = self.lag_of(&entity);
        let rest = self.runnable_weight();
        self.on_stop(lag, rest);
        entity.weight = weight;
        self.on_start(&mut entity, lag, rest);
        entity.vd = entity.v + virt(entity.request, weight);
        self.queue.push_back(Queued { entity, ..q });
        self.total += weight;
        true
    }

    /// Gives the running thread `weight` in the same way, and returns
    /// whether there is one.
    pub fn reweigh_running(&mut self, weight: u64) -> bool {
        let Some((id, mut entity)) = self.running.take() else {
            return false;
        };
        let lag = self.lag_of(&entity);
        let rest = self.runnable_weight();
        self.on_stop(lag, rest);
        entity.weight = weight;
        self.on_start(&mut entity, lag, rest);
        self.running = Some((id, entity));
        true
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

    /// Takes the thread to run next and makes it the running one: the
    /// one whose deadline came first if any has come, otherwise the
    /// eligible one with the earliest virtual deadline. The running
    /// thread must have been queued again or stopped first.
    pub fn pick(&mut self) -> Option<(T, Entity)> {
        debug_assert!(self.running.is_none());
        let due = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, q)| q.deadline <= self.elapsed)
            .min_by_key(|(_, q)| q.deadline)
            .map(|(i, _)| i);
        let i = due.or_else(|| {
            self.queue
                .iter()
                .enumerate()
                .filter(|(_, q)| q.entity.v <= self.vtime)
                .min_by_key(|(_, q)| q.entity.vd)
                .map(|(i, _)| i)
        });
        // Some thread is eligible, as the lags add up to zero; the one
        // furthest behind is taken should rounding hide it.
        let i = i.or_else(|| {
            self.queue
                .iter()
                .enumerate()
                .min_by_key(|(_, q)| q.entity.v)
                .map(|(i, _)| i)
        })?;
        let q = self.queue.remove(i).expect("found above");
        self.total -= q.entity.weight;
        let mut entity = q.entity;
        entity.ran = true;
        if due.is_some() && entity.v > self.vtime {
            entity.request = MIN_SLICE;
        }
        self.running = Some((q.id, entity));
        Some((q.id, entity))
    }

    /// How long the running thread may run before the next thread's
    /// turn.
    pub fn slice(&self) -> i64 {
        let request = self.running.map_or(REQUEST_MAX, |(_, e)| e.request);
        request.min(self.until_due())
    }

    /// Accounts for `ran` nanoseconds that the running thread ran.
    pub fn charge(&mut self, ran: i64) {
        let ran = ran.max(0);
        let total = self.runnable_weight();
        if let Some((_, entity)) = &mut self.running {
            entity.v += virt(ran, entity.weight);
            self.vtime += virt(ran, total);
            self.elapsed += ran;
        }
    }

    /// The lag of runnable thread `id`, for tests.
    pub fn lag(&self, id: T) -> Option<i64> {
        let entity = match self.running {
            Some((r, e)) if r == id => e,
            _ => self.queue.iter().find(|q| q.id == id)?.entity,
        };
        Some((self.lag_of(&entity) >> SHIFT) as i64)
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
