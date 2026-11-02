//! Pagers, which supply the pages of paged memory objects, and the
//! threads waiting for those pages.

use crate::memory_object::MemoryObject;
use crate::sched::{self, Thread};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use kernel_core::object::budget::Budget;
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

pub struct Pager {
    header: Header,
    budget: Arc<Budget>,
    /// The paged objects the pager supplies.
    objects: SpinLock<Vec<Weak<MemoryObject>>>,
}

impl Pager {
    const COST: u64 = size_of::<Pager>() as u64;

    /// A pager charged to `budget`.
    pub fn new(budget: &Arc<Budget>) -> Result<Arc<Pager>, Error> {
        budget.charge_kernel(Self::COST)?;
        kernel_core::fallible::try_arc(Pager {
            header: Header::new(ObjectType::Pager),
            budget: budget.clone(),
            objects: SpinLock::new(Vec::new()),
        })
    }

    /// Records `mem` as one of the pager's objects.
    pub fn add(&self, mem: &Arc<MemoryObject>) -> Result<(), Error> {
        let mut objects = self.objects.lock();
        objects.retain(|o| o.strong_count() > 0);
        objects.try_reserve(1).map_err(|_| Error::NoMemory)?;
        objects.push(Arc::downgrade(mem));
        Ok(())
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        // Every unanswered and later request of its objects fails.
        let objects = core::mem::take(&mut *self.objects.lock());
        for mem in objects.iter().filter_map(Weak::upgrade) {
            mem.pager_gone();
        }
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Pager {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// A thread waiting for a page: the object's ID and the page index.
struct Waiter {
    koid: u64,
    index: u64,
    thread: Arc<Thread>,
}

/// The threads waiting for pages. The lock comes after an object's pages
/// lock and before the scheduler's.
static WAITERS: SpinLock<Vec<Waiter>> = SpinLock::new(Vec::new());

/// Registers the running thread as waiting for page `index` of the
/// object with ID `koid`.
pub fn wait_for(koid: u64, index: u64) -> Result<Arc<Thread>, Error> {
    let thread = sched::current().ok_or(Error::BadState)?;
    let mut waiters = WAITERS.lock();
    waiters.try_reserve(1).map_err(|_| Error::NoMemory)?;
    waiters.push(Waiter {
        koid,
        index,
        thread: thread.clone(),
    });
    Ok(thread)
}

/// Ends the waits of `thread` for the object with ID `koid`.
pub fn stop_waiting(koid: u64, thread: &Arc<Thread>) {
    WAITERS
        .lock()
        .retain(|w| !(w.koid == koid && Arc::ptr_eq(&w.thread, thread)));
}

/// Wakes the threads waiting for pages from `first` to `end` of the
/// object with ID `koid`, which may have changed.
pub fn wake(koid: u64, first: u64, end: u64) {
    for w in WAITERS.lock().iter() {
        if w.koid == koid && (first..end).contains(&w.index) {
            sched::wake_sleeper(&w.thread);
        }
    }
}
