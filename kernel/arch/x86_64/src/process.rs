//! Processes: an address space, a handle table and threads.

use crate::aspace::AddressSpace;
use crate::sched::{self, Thread};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use kernel_core::object::budget::Budget;
use kernel_core::object::handles::{Entry, HANDLE_INVALID, Handle, HandleTable};
use kernel_core::object::{Header, Name, Object, ObjectType};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

pub const NEW: u32 = 0;
pub const RUNNING: u32 = 1;
pub const TERMINATED: u32 = 2;

pub struct Process {
    header: Header,
    name: Name,
    budget: Arc<Budget>,
    /// Kernel memory charged to the budget for the process itself.
    charged: u64,
    aspace: AddressSpace,
    handles: SpinLock<HandleTable>,
    state: AtomicU32,
    exit_code: AtomicI64,
    /// The threads created in the process. Threads keep their process
    /// alive, not the other way round.
    threads: SpinLock<Vec<Weak<Thread>>>,
}

impl Process {
    /// A process in state `NEW` with no mappings, handles or threads,
    /// belonging to `budget`. Its memory is charged to the budget when
    /// `charge` is set.
    pub fn new(budget: &Arc<Budget>, name: Name, charge: bool) -> Result<Arc<Process>, Error> {
        let charged = if charge {
            let size = size_of::<Process>() as u64;
            budget.charge_kernel(size)?;
            size
        } else {
            0
        };
        let aspace = match AddressSpace::new(budget.clone(), charge) {
            Ok(aspace) => aspace,
            Err(e) => {
                budget.release_kernel(charged);
                return Err(e);
            }
        };
        budget.add_process();
        Ok(Arc::new(Process {
            header: Header::new(ObjectType::Process),
            name,
            budget: budget.clone(),
            charged,
            aspace,
            handles: SpinLock::new(HandleTable::new()),
            state: AtomicU32::new(NEW),
            exit_code: AtomicI64::new(0),
            threads: SpinLock::new(Vec::new()),
        }))
    }

    pub fn name(&self) -> &Name {
        &self.name
    }

    pub fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    pub fn aspace(&self) -> &AddressSpace {
        &self.aspace
    }

    pub fn handles(&self) -> &SpinLock<HandleTable> {
        &self.handles
    }

    pub fn state(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    pub fn exit_code(&self) -> i64 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Creates a thread in state `NEW` that belongs to `process`. Fails
    /// with `BadState` when the process has terminated.
    pub fn create_thread(
        process: &Arc<Process>,
        name: Name,
        charge: bool,
    ) -> Result<Arc<Thread>, Error> {
        let mut threads = process.threads.lock();
        if process.state() == TERMINATED {
            return Err(Error::BadState);
        }
        threads.retain(|t| t.strong_count() > 0);
        threads.try_reserve(1).map_err(|_| Error::NoMemory)?;
        let thread = sched::Thread::new_user(process.clone(), name, charge)?;
        threads.push(Arc::downgrade(&thread));
        Ok(thread)
    }

    /// Moves `arg` into the handle table of `process`, sets it `RUNNING`
    /// and starts `thread` at `entry` with stack pointer `stack`, the new
    /// handle value (or `HANDLE_INVALID` without `arg`) and `arg2` as its
    /// arguments. Fails with `BadState` unless the process is `NEW` and
    /// `thread` is one of its threads and `NEW`. On failure `arg` is
    /// returned with the error, so that the caller keeps it. A new handle
    /// table slot for `arg` is charged to `budget`, if given.
    pub fn start(
        process: &Arc<Process>,
        thread: &Arc<Thread>,
        entry: u64,
        stack: u64,
        arg: Option<Entry>,
        arg2: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), (Error, Option<Entry>)> {
        let threads = process.threads.lock();
        let ours = thread.process().is_some_and(|p| Arc::ptr_eq(p, process));
        if process.state() != NEW || !ours || thread.state() != sched::NEW {
            return Err((Error::BadState, arg));
        }
        let handle: Handle = match arg {
            Some(entry) => {
                let inserted =
                    process
                        .handles
                        .lock()
                        .insert(entry.object.clone(), entry.rights, budget);
                inserted.map_err(|e| (e, Some(entry)))?
            }
            None => HANDLE_INVALID,
        };
        process.state.store(RUNNING, Ordering::Release);
        Thread::start_user(thread, entry, stack, u64::from(handle), arg2);
        drop(threads);
        Ok(())
    }

    /// Starts a further `NEW` thread of a `RUNNING` process at `entry`
    /// with stack pointer `stack` and the two arguments, or fails with
    /// `BadState`.
    pub fn start_thread(
        thread: &Arc<Thread>,
        entry: u64,
        stack: u64,
        arg1: u64,
        arg2: u64,
    ) -> Result<(), Error> {
        let Some(process) = thread.process() else {
            return Err(Error::BadState);
        };
        let _threads = process.threads.lock();
        if thread.state() != sched::NEW || process.state() != RUNNING {
            return Err(Error::BadState);
        }
        Thread::start_user(thread, entry, stack, arg1, arg2);
        Ok(())
    }
}

impl Object for Process {
    fn header(&self) -> &Header {
        &self.header
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.budget.remove_process();
        self.budget.release_kernel(self.charged);
    }
}
