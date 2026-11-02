//! `Timer` objects, which set `SIGNALED` at a deadline.

use alloc::sync::{Arc, Weak};
use kernel_core::object::budget::Budget;
use kernel_core::object::{Header, Object, ObjectType, signals};
use kernel_core::status::Error;

pub struct Timer {
    header: Header,
    budget: Arc<Budget>,
}

impl Timer {
    const COST: u64 = size_of::<Timer>() as u64;

    /// A timer with no deadline, charged to `budget`.
    pub fn new(budget: &Arc<Budget>) -> Result<Arc<Timer>, Error> {
        budget.charge_kernel(Self::COST)?;
        kernel_core::fallible::try_arc(Timer {
            header: Header::new(ObjectType::Timer),
            budget: budget.clone(),
        })
    }

    /// Clears `SIGNALED` and sets it at `deadline`, replacing an earlier
    /// deadline. A deadline that has passed sets it at once.
    pub fn set(self: &Arc<Self>, deadline: i64) -> Result<(), Error> {
        self.header.update_signals(signals::SIGNALED, 0);
        let armed = Armed(Arc::downgrade(self));
        if deadline <= crate::time::now() {
            crate::sched::disarm_timer(&armed);
            self.header.update_signals(0, signals::SIGNALED);
            return Ok(());
        }
        crate::sched::arm_timer(armed, deadline)
    }

    /// Clears `SIGNALED` and removes the deadline.
    pub fn cancel(self: &Arc<Self>) {
        crate::sched::disarm_timer(&Armed(Arc::downgrade(self)));
        self.header.update_signals(signals::SIGNALED, 0);
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Timer {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// An armed timer in the scheduler's deadlines. A timer that is gone by
/// its deadline is skipped.
pub struct Armed(Weak<Timer>);

impl PartialEq for Armed {
    fn eq(&self, other: &Armed) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl Armed {
    /// Sets `SIGNALED` on the timer, if it still exists.
    pub fn fire(self) {
        if let Some(timer) = self.0.upgrade() {
            timer.header.update_signals(0, signals::SIGNALED);
        }
    }
}
