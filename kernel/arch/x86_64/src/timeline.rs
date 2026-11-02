//! Timelines: values that only grow, and the threads waiting for them.

use crate::process::Process;
use crate::sched::{self, Thread};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel_core::object::budget::Budget;
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

pub struct Timeline {
    header: Header,
    budget: Arc<Budget>,
    value: AtomicU64,
}

/// A thread in `timeline_wait`: the timeline's object ID and the value.
struct Waiter {
    koid: u64,
    value: u64,
    thread: Arc<Thread>,
}

static WAITERS: SpinLock<Vec<Waiter>> = SpinLock::new(Vec::new());

impl Timeline {
    const COST: u64 = size_of::<Timeline>() as u64;

    /// A timeline at `initial`, charged to `budget`.
    pub fn new(budget: &Arc<Budget>, initial: u64) -> Result<Arc<Timeline>, Error> {
        budget.charge_kernel(Self::COST)?;
        kernel_core::fallible::try_arc(Timeline {
            header: Header::new(ObjectType::Timeline),
            budget: budget.clone(),
            value: AtomicU64::new(initial),
        })
    }

    pub fn value(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    /// Raises the value to `value`, waking the waits it reaches, or fails
    /// with `InvalidArgs` when `value` is lower.
    pub fn signal(&self, value: u64) -> Result<(), Error> {
        if value < self.value() {
            return Err(Error::InvalidArgs);
        }
        self.value.store(value, Ordering::Release);
        let koid = self.header.koid();
        for w in WAITERS.lock().iter() {
            if w.koid == koid && w.value <= value {
                sched::wake_sleeper(&w.thread);
            }
        }
        crate::port::timeline_raised(koid, value);
        Ok(())
    }

    /// Blocks the running thread until the value is at least `value`, as
    /// `timeline_wait` does.
    pub fn wait(&self, process: &Process, value: u64, deadline: i64) -> Result<(), Error> {
        let thread = sched::current().ok_or(Error::BadState)?;
        let koid = self.header.koid();
        {
            let mut waiters = WAITERS.lock();
            waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
            waiters.push(Waiter {
                koid,
                value,
                thread: thread.clone(),
            });
        }
        let result = loop {
            if self.value() >= value {
                break Ok(());
            }
            if crate::signal::interrupted(&thread, process) {
                break Err(Error::Interrupted);
            }
            if thread.state() == sched::DEAD || deadline <= crate::time::now() {
                break Err(Error::TimedOut);
            }
            if let Err(e) = sched::wait_for_signals(&thread, deadline) {
                break Err(e);
            }
        };
        WAITERS
            .lock()
            .retain(|w| !(w.koid == koid && Arc::ptr_eq(&w.thread, &thread)));
        result
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Timeline {
    fn header(&self) -> &Header {
        &self.header
    }
}
