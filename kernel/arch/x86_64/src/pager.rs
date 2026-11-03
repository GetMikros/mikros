//! Pagers, which supply the pages of paged memory objects, and the
//! threads waiting for those pages.

use crate::memory_object::MemoryObject;
use crate::publish::ITEM_STEP;
use crate::sched::{self, Thread};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use kernel_core::nodes::{self, Map};
use kernel_core::object::budget::{Budget, Held};
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

pub struct Pager {
    header: Header,
    budget: Arc<Budget>,
    objects: SpinLock<Objects>,
}

/// The paged objects a pager supplies, among them some that are gone.
#[derive(Default)]
struct Objects {
    list: Vec<Weak<MemoryObject>>,
    /// The entry that `add` looks at next.
    next: usize,
}

impl Pager {
    const COST: u64 = size_of::<Pager>() as u64;

    /// A pager charged to `budget`.
    pub fn new(budget: &Arc<Budget>) -> Result<Arc<Pager>, Error> {
        budget.charge_kernel(Self::COST)?;
        kernel_core::fallible::try_arc(Pager {
            header: Header::new(ObjectType::Pager),
            budget: budget.clone(),
            objects: SpinLock::new(Objects::default()),
        })
    }

    /// Records `mem` as one of the pager's objects. Each call looks at
    /// two entries in turn and drops those of objects that are gone, so
    /// that the list stays within about twice the objects alive without
    /// a walk over all of it.
    pub fn add(&self, mem: &Arc<MemoryObject>) -> Result<(), Error> {
        let mut objects = self.objects.lock();
        let Objects { list, next } = &mut *objects;
        for _ in 0..2 {
            if list.is_empty() {
                break;
            }
            let i = *next % list.len();
            if list[i].strong_count() == 0 {
                list.swap_remove(i);
            } else {
                *next = i + 1;
            }
        }
        list.try_reserve(1).map_err(|_| Error::NoMemory)?;
        list.push(Arc::downgrade(mem));
        Ok(())
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        // Every unanswered and later request of its objects fails. The
        // objects are visited with a reschedule check every `ITEM_STEP`
        // when no lock is held.
        let objects = core::mem::take(&mut self.objects.lock().list);
        for (i, mem) in objects.iter().filter_map(Weak::upgrade).enumerate() {
            mem.pager_gone();
            if (i + 1) % ITEM_STEP == 0 {
                sched::preempt_point_if_unlocked();
            }
        }
        self.budget.release_kernel(Self::COST);
    }
}

impl Object for Pager {
    fn header(&self) -> &Header {
        &self.header
    }
}

/// The index of the page a thread waits for, and the thread.
type Waiter = (u64, Arc<Thread>);

/// The threads waiting for pages, by the ID of the object and of the
/// thread. A thread waits for one page at a time. The lock comes after an
/// object's pages lock and before the scheduler's.
static WAITERS: SpinLock<Map<(u64, u64), Waiter>> = SpinLock::new(nodes::map());

/// The kernel memory charged to a process while one of its threads waits
/// for a page: its entry in `WAITERS` and that entry's share of the
/// tree's nodes.
const WAITER_COST: u64 = 2 * size_of::<((u64, u64), Waiter)>() as u64;

/// Charges the process of the running thread, if it has one, for a wait
/// for a page, until the result is dropped.
pub fn charge_wait() -> Result<Option<Held>, Error> {
    let thread = sched::current().ok_or(Error::BadState)?;
    thread
        .process()
        .map(|p| p.budget().hold_kernel(WAITER_COST))
        .transpose()
}

/// Registers the running thread as waiting for page `index` of the
/// object with ID `koid`.
pub fn wait_for(koid: u64, index: u64) -> Result<Arc<Thread>, Error> {
    let thread = sched::current().ok_or(Error::BadState)?;
    let _claim = nodes::claim_insert()?;
    let key = (koid, thread.header().koid());
    WAITERS.lock().insert(key, (index, thread.clone()));
    Ok(thread)
}

/// Ends the wait of `thread` for the object with ID `koid`.
pub fn stop_waiting(koid: u64, thread: &Arc<Thread>) {
    WAITERS.lock().remove(&(koid, thread.header().koid()));
}

/// Wakes the threads waiting for pages from `first` to `end` of the
/// object with ID `koid`, which may have changed. The object's waiters are
/// visited in steps of `ITEM_STEP`, with a reschedule check between two
/// when no lock is held; one that registers meanwhile saw the change.
pub fn wake(koid: u64, first: u64, end: u64) {
    let mut from = 0;
    loop {
        let more = {
            let waiters = WAITERS.lock();
            let mut step = waiters.range((koid, from)..=(koid, u64::MAX));
            for (&(_, thread), (index, waiter)) in step.by_ref().take(ITEM_STEP) {
                if (first..end).contains(index) {
                    sched::wake_sleeper(waiter);
                }
                from = thread.saturating_add(1);
            }
            step.next().is_some()
        };
        if !more {
            return;
        }
        sched::preempt_point_if_unlocked();
    }
}
