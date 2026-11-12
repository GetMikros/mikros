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

    /// The queued threads, in no particular order.
    pub fn queued(&self) -> impl Iterator<Item = T> + '_ {
        self.queue.iter().map(|q| q.id)
    }

    /// The total weight of the runnable threads.
    pub fn runnable_weight(&self) -> u64 {
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
    /// deadline, queues it under `id` and returns it as queued.
    fn insert(&mut self, id: T, mut entity: Entity) -> Entity {
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
        entity
    }

    /// Makes a thread that is new or stopped runnable and queues it. It
    /// starts with the lag it stopped with when that was not positive.
    /// When it is the running thread, which was made runnable again
    /// before it stopped, it is queued when it stops instead. Returns
    /// whether a thread is running that the queue would now pick it
    /// before: it is eligible, with an earlier virtual deadline.
    pub fn enqueue(&mut self, id: T, entity: Entity) -> bool {
        if self.running.is_some_and(|(r, _)| r == id) {
            self.rewoken = Some(id);
            return false;
        }
        let mut entity = entity;
        let lag = if entity.ran && entity.lag <= 0 {
            entity.lag
        } else {
            0
        };
        let others = self.runnable_weight();
        self.on_start(&mut entity, lag, others);
        let entity = self.insert(id, entity);
        self.running
            .is_some_and(|(_, r)| entity.v <= self.vtime && entity.vd < r.vd)
    }

    /// Queues the running thread again, under `id`, as it stays runnable.
    pub fn requeue(&mut self, id: T) {
        // A thread made runnable again while it ran stops before it can
        // be preempted, which takes the record.
        debug_assert!(
            self.rewoken.is_none(),
            "a running thread was made runnable again"
        );
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
    use std::vec::Vec;

    /// The bound on lags.
    const LAG_MAX: i64 = 10_000_000;

    #[test]
    fn weights_follow_the_formula() {
        for p in 0..=32 {
            let expected = (1024.0 * 1.25f64.powi(p as i32 - 16)).round() as u64;
            assert_eq!(weight(p), expected, "priority {p}");
        }
    }

    /// Checks that every runnable thread's lag is within the bound.
    fn check_lags(fair: &Fair<u32>, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            if let Some(lag) = fair.lag(id) {
                assert!(lag.abs() <= LAG_MAX, "thread {id} has lag {lag}");
            }
        }
    }

    /// Runs `priorities` for `duration` ns, all always runnable, and
    /// checks each thread's lag, its time against its share and the gaps
    /// between its runs.
    fn simulate(priorities: &[u32], duration: i64) {
        let n = priorities.len();
        let mut fair = Fair::new();
        let mut ran = vec![0i64; n];
        let mut last_end = vec![0i64; n];
        let mut max_gap = vec![0i64; n];
        for (i, &p) in priorities.iter().enumerate() {
            fair.enqueue(i as u32, Entity::new(p));
        }
        let total: u64 = priorities.iter().map(|&p| weight(p)).sum();
        let interval = wait_limit(n);
        let mut now = 0i64;
        while now < duration {
            let (id, _) = fair.pick().unwrap();
            let id = id as usize;
            let slice = fair.slice();
            max_gap[id] = max_gap[id].max(now - last_end[id]);
            now += slice;
            ran[id] += slice;
            last_end[id] = now;
            fair.charge(slice);
            check_lags(&fair, 0..n as u32);
            fair.requeue(id as u32);
            for (j, &p) in priorities.iter().enumerate() {
                let share = (i128::from(now) * i128::from(weight(p)) / i128::from(total)) as i64;
                assert!(
                    (ran[j] - share).abs() <= LAG_MAX,
                    "thread {j} at {now}: ran {} for a share of {share}",
                    ran[j]
                );
            }
        }
        // Threads whose deadlines come together start 1 ns apart.
        for (j, &gap) in max_gap.iter().enumerate() {
            assert!(gap <= interval + n as i64, "thread {j} waited {gap} ns");
        }
    }

    #[test]
    fn shares_by_weight() {
        simulate(&[16, 16], 1_000_000_000);
        simulate(&[16, 17, 20], 1_000_000_000);
        simulate(&[0, 31], 1_000_000_000);
    }

    #[test]
    fn holds_for_many_mixes_of_priorities() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..50 {
            let n = 1 + (next() % 24) as usize;
            let priorities: Vec<u32> = (0..n).map(|_| (next() % 32) as u32).collect();
            simulate(&priorities, 300_000_000);
        }
    }

    /// Runs the next thread for its slice, or for `ran` if that is
    /// shorter, then queues it again, or stops it when `block` is set and
    /// returns its state.
    fn run(fair: &mut Fair<u32>, ran: Option<i64>, block: bool) -> (u32, i64, Option<Entity>) {
        let (id, _) = fair.pick().unwrap();
        let slice = fair.slice();
        let ran = ran.map_or(slice, |r| r.min(slice));
        fair.charge(ran);
        if block {
            (id, ran, fair.stop())
        } else {
            fair.requeue(id);
            (id, ran, None)
        }
    }

    #[test]
    fn keeps_lags_bounded_while_a_thread_keeps_blocking() {
        // Threads 0 to 2 are always runnable; thread 3 runs, blocks and
        // is woken again after the next run, over and over.
        let mut fair = Fair::new();
        for (id, p) in [(0, 16), (1, 25), (2, 10), (3, 16)] {
            fair.enqueue(id, Entity::new(p));
        }
        let mut sleeping = None;
        for _ in 0..20_000 {
            let (id, _) = fair.pick().unwrap();
            fair.charge(fair.slice());
            let stopped = if id == 3 {
                fair.stop()
            } else {
                fair.requeue(id);
                None
            };
            if let Some(e) = sleeping.take() {
                fair.enqueue(3, e);
            }
            sleeping = stopped;
            check_lags(&fair, 0..4);
        }
    }

    #[test]
    fn gives_no_credit_for_blocked_time() {
        let mut fair = Fair::new();
        fair.enqueue(1, Entity::new(16));
        fair.enqueue(2, Entity::new(16));
        // Thread 1 runs a little and blocks, then thread 2 runs alone for
        // a second.
        let (first, _, _) = run(&mut fair, Some(1_000), true);
        let blocked = if first == 1 { 1 } else { 2 };
        let stopped = fair.lag(blocked);
        assert!(stopped.is_none());
        let other = 3 - blocked;
        for _ in 0..300 {
            run(&mut fair, None, false);
        }
        // A blocked thread that was ahead keeps that; one that was behind
        // starts again at zero.
        let mut ahead = Entity::new(16);
        ahead.ran = true;
        ahead.lag = -2_000_000 << SHIFT;
        fair.enqueue(blocked, ahead);
        let lag = fair.lag(blocked).unwrap();
        assert!((-2_000_000..-900_000).contains(&lag), "lag {lag}");
        let _ = fair.remove(blocked);
        let mut behind = ahead;
        behind.lag = 5_000_000 << SHIFT;
        fair.enqueue(blocked, behind);
        assert_eq!(fair.lag(blocked), Some(0));
        check_lags(&fair, [other]);
    }

    #[test]
    fn bounds_the_wait_when_the_queue_shrinks() {
        let mut fair = Fair::new();
        for id in [1, 2, 3, 4] {
            fair.enqueue(id, Entity::new(16));
        }
        // Three threads run whole slices and block; the fourth waits no
        // more than one period.
        let mut waited = 0;
        let mut ran_ids = Vec::new();
        for _ in 0..3 {
            let (id, ran, _) = run(&mut fair, None, true);
            ran_ids.push(id);
            waited += ran;
        }
        assert!(waited <= wait_limit(4), "waited {waited} ns");
        check_lags(&fair, 1..=4);
    }

    #[test]
    fn handles_a_weight_change_as_a_stop_and_a_start() {
        let mut fair = Fair::new();
        for id in [1, 2, 3] {
            fair.enqueue(id, Entity::new(16));
        }
        for _ in 0..10 {
            run(&mut fair, None, false);
        }
        let queued = (1..=3).find(|&id| fair.lag(id).is_some()).unwrap();
        let before = fair.lag(queued).unwrap();
        assert!(fair.reweigh(queued, weight(25)));
        // The thread keeps its lag up to the share of it that the moved
        // virtual time takes back.
        let after = fair.lag(queued).unwrap();
        assert!(
            (after - before).abs() <= before.abs(),
            "{before} -> {after}"
        );
        for _ in 0..1000 {
            run(&mut fair, None, false);
            check_lags(&fair, 1..=3);
        }
    }

    #[test]
    fn handles_a_weight_change_of_the_running_thread() {
        let mut fair = Fair::new();
        for id in [1, 2, 3] {
            fair.enqueue(id, Entity::new(16));
        }
        for _ in 0..10 {
            run(&mut fair, None, false);
        }
        assert!(!fair.reweigh_running(weight(25)));
        let (id, _) = fair.pick().unwrap();
        fair.charge(fair.slice() / 2);
        let before = fair.lag(id).unwrap();
        assert!(fair.reweigh_running(weight(25)));
        let after = fair.lag(id).unwrap();
        assert!(
            (after - before).abs() <= before.abs(),
            "{before} -> {after}"
        );
        fair.charge(fair.slice());
        fair.requeue(id);
        // The heavier thread now gets the larger share.
        let mut ran = [0i64; 4];
        for _ in 0..3000 {
            let (id, ran_now, _) = run(&mut fair, None, false);
            ran[id as usize] += ran_now;
            check_lags(&fair, 1..=3);
        }
        let others = ran
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != 0 && i as u32 != id);
        assert!(others.into_iter().all(|(_, &r)| ran[id as usize] > 2 * r));
    }

    #[test]
    fn queues_the_running_thread_woken_before_it_stops() {
        let mut fair = Fair::new();
        fair.enqueue(1, Entity::new(16));
        let (id, _) = fair.pick().unwrap();
        fair.charge(1_000);
        // Woken while it still runs, as by an interrupt before it
        // switched out.
        fair.enqueue(id, Entity::new(16));
        assert!(fair.stop().is_none());
        assert_eq!(fair.len(), 1);
        assert_eq!(fair.pick().map(|(id, _)| id), Some(1));
    }

    #[test]
    fn tells_when_a_queued_thread_comes_before_the_running_one() {
        let mut fair = Fair::new();
        // No thread runs yet.
        assert!(!fair.enqueue(1, Entity::new(16)));
        let (running, _) = fair.pick().unwrap();
        fair.charge(1_000);
        // One of the same weight gets the same request from a later
        // virtual time, so a later virtual deadline.
        assert!(!fair.enqueue(2, Entity::new(16)));
        // A heavier one gets an earlier one.
        assert!(fair.enqueue(3, Entity::new(31)));
        // One that stopped ahead of its share is not eligible.
        let mut ahead = Entity::new(31);
        ahead.ran = true;
        ahead.lag = -1_000_000 << SHIFT;
        assert!(!fair.enqueue(4, ahead));
        // The running thread itself is queued only when it stops.
        assert!(!fair.enqueue(running, Entity::new(31)));
    }
}
