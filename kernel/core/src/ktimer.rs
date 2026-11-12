//! `Timer` objects, which set `SIGNALED` at a deadline.

use crate::object::budget::Budget;
use crate::object::{Header, Object, ObjectType, signals};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};

pub struct Timer {
    header: Header,
    budget: Arc<Budget>,
    state: SpinLock<State>,
}

/// Where a timer's deadline is: the processor whose list holds it, if
/// any, and the number of the latest `set` or `cancel`, which a deadline
/// must carry to fire.
struct State {
    cpu: Option<usize>,
    generation: u64,
}

impl Timer {
    /// The timer and its armed entry with that entry's share of storage.
    const COST: u64 =
        (size_of::<Timer>() + 2 * crate::deadline::Deadlines::<Armed>::ENTRY_SIZE) as u64;

    /// A timer with no deadline, charged to `budget`.
    pub fn new(budget: &Arc<Budget>) -> Result<Arc<Timer>, Error> {
        budget.charge_kernel(Self::COST)?;
        crate::fallible::try_arc(Timer {
            header: Header::new(ObjectType::Timer),
            budget: budget.clone(),
            state: SpinLock::new(
                crate::lock_order::TIMER,
                State {
                    cpu: None,
                    generation: 0,
                },
            ),
        })
    }

    /// Clears `SIGNALED` and sets it at `deadline`, replacing an earlier
    /// deadline. A deadline that has passed sets it at once.
    pub fn set(self: &Arc<Self>, deadline: i64) -> Result<(), Error> {
        let mut state = self.state.lock();
        self.disarm(&mut state);
        self.header.update_signals(signals::SIGNALED, 0);
        if deadline <= crate::arch::monotonic_now() {
            self.header.update_signals(0, signals::SIGNALED);
            return Ok(());
        }
        // The deadline goes on this processor's list, whose timer it arms.
        let armed = Armed(Arc::downgrade(self), state.generation);
        crate::sched::arm_timer(armed, deadline)?;
        state.cpu = Some(crate::arch::cpu_index() as usize);
        Ok(())
    }

    /// Clears `SIGNALED` and removes the deadline.
    pub fn cancel(self: &Arc<Self>) {
        let mut state = self.state.lock();
        self.disarm(&mut state);
        self.header.update_signals(signals::SIGNALED, 0);
    }

    /// Removes the deadline from the list that holds it, and makes one
    /// that has left it already, about to fire, not fire.
    fn disarm(self: &Arc<Self>, state: &mut State) {
        state.generation += 1;
        if let Some(cpu) = state.cpu.take() {
            crate::sched::disarm_timer(cpu, &Armed(Arc::downgrade(self), 0));
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // A deadline left in a processor's list would keep the timer's
        // memory until it passes, which for `INFINITE` is never.
        if let Some(cpu) = self.state.get_mut().cpu.take() {
            crate::sched::forget_timer(cpu, self);
        }
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Timer {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// An armed timer in the scheduler's deadlines, with the generation it
/// was armed in. A timer that is gone by its deadline is skipped.
pub struct Armed(Weak<Timer>, u64);

impl PartialEq for Armed {
    fn eq(&self, other: &Armed) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl Armed {
    /// Whether this is the entry of `timer`.
    pub fn is_of(&self, timer: *const Timer) -> bool {
        core::ptr::eq(Weak::as_ptr(&self.0), timer)
    }

    /// Sets `SIGNALED` on the timer, if it still exists and was not set
    /// or cancelled again since it was armed.
    pub fn fire(self) {
        if let Some(timer) = self.0.upgrade() {
            let mut state = timer.state.lock();
            if state.generation == self.1 {
                state.cpu = None;
                timer.header.update_signals(0, signals::SIGNALED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deadline::INFINITE;

    #[test]
    fn a_dropped_timer_leaves_no_deadline_behind() {
        let _mock = crate::arch::mock::lock();
        let budget = Budget::root();
        let before = crate::sched::armed_timers(0);
        let timer = Timer::new(&budget).unwrap();
        timer.set(INFINITE).unwrap();
        assert_eq!(crate::sched::armed_timers(0), before + 1);
        drop(timer);
        assert_eq!(crate::sched::armed_timers(0), before);
    }

    #[test]
    fn the_timer_interrupt_arms_no_deadline_that_has_passed() {
        let _mock = crate::arch::mock::lock();
        let budget = Budget::root();
        crate::arch::mock::set_now(1_000);
        let expiring = Timer::new(&budget).unwrap();
        let waiting = Timer::new(&budget).unwrap();
        expiring.set(2_000).unwrap();
        waiting.set(3_000).unwrap();
        crate::arch::mock::set_now(2_500);
        crate::sched::timer_interrupt();
        assert_ne!(expiring.header.signals() & signals::SIGNALED, 0);
        assert_eq!(waiting.header.signals() & signals::SIGNALED, 0);
        // Armed for the expired deadline, the timer would interrupt again
        // at once.
        let armed = crate::arch::mock::timer_deadline();
        assert!(armed.is_some_and(|at| at > 2_500), "armed for {armed:?}");
    }
}
