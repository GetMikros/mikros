//! Processes: an address space, a handle table and threads.

use crate::aspace::{AddressSpace, USER_END, USER_START};
use crate::sched::{self, Thread};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use kernel_core::exception::DEBUG_EVENTS;
use kernel_core::object::budget::Budget;
use kernel_core::object::channel::{Endpoint, PeerObserver};
use kernel_core::object::handles::{Entry, HANDLE_INVALID, Handle, HandleTable};
use kernel_core::object::{Header, Name, Object, ObjectType, signals};
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
    aspace: Arc<AddressSpace>,
    handles: SpinLock<HandleTable>,
    state: AtomicU32,
    exit_code: AtomicI64,
    /// The threads created in the process. Threads keep their process
    /// alive, not the other way round.
    threads: SpinLock<Vec<Weak<Thread>>>,
    /// The started threads that are not DEAD, changed under `threads`.
    started: AtomicU32,
    exception: SpinLock<Option<ExceptionChannel>>,
}

/// The endpoint of a process's exception channel that the kernel keeps.
struct ExceptionChannel {
    end: Arc<Endpoint>,
    options: u32,
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
            aspace: Arc::new(aspace),
            handles: SpinLock::new(HandleTable::new()),
            state: AtomicU32::new(NEW),
            exit_code: AtomicI64::new(0),
            threads: SpinLock::new(Vec::new()),
            started: AtomicU32::new(0),
            exception: SpinLock::new(None),
        }))
    }

    pub fn name(&self) -> &Name {
        &self.name
    }

    pub fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    pub fn aspace(&self) -> &Arc<AddressSpace> {
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

    /// The kernel's endpoint of the exception channel, while the returned
    /// endpoint is open.
    pub fn exception_channel(&self) -> Option<Arc<Endpoint>> {
        let exception = self.exception.lock();
        let channel = exception.as_ref().filter(|c| !c.end.peer_closed())?;
        Some(channel.end.clone())
    }

    /// Whether the exception channel asks for thread start and exit
    /// reports.
    #[allow(dead_code, reason = "thread start and exit reports come later")]
    pub fn debug_events(&self) -> bool {
        let exception = self.exception.lock();
        exception
            .as_ref()
            .is_some_and(|c| !c.end.peer_closed() && c.options & DEBUG_EVENTS != 0)
    }

    /// Creates the exception channel with `options`, charged to `budget`,
    /// and returns the endpoint for user space. Fails with
    /// `AlreadyExists` while an earlier one is open.
    pub fn create_exception_channel(
        process: &Arc<Process>,
        options: u32,
        budget: &Arc<Budget>,
    ) -> Result<Arc<Endpoint>, Error> {
        let mut exception = process.exception.lock();
        if exception.as_ref().is_some_and(|c| !c.end.peer_closed()) {
            return Err(Error::AlreadyExists);
        }
        let (end, user) = Endpoint::pair(Some(budget))?;
        let observer: Weak<dyn PeerObserver> = Arc::downgrade(process) as _;
        end.set_observer(observer);
        // A replaced channel's kernel endpoint is dropped after the lock.
        let old = exception.replace(ExceptionChannel { end, options });
        drop(exception);
        drop(old);
        Ok(user)
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
    /// `thread` is one of its threads and `NEW`. A new handle table slot
    /// for `arg` is charged to `budget`, if given.
    pub fn start(
        process: &Arc<Process>,
        thread: &Arc<Thread>,
        entry: u64,
        stack: u64,
        arg: Option<Entry>,
        arg2: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), Error> {
        let threads = process.threads.lock();
        let ours = thread.process().is_some_and(|p| Arc::ptr_eq(p, process));
        if process.state() != NEW || !ours || thread.state() != sched::NEW {
            return Err(Error::BadState);
        }
        let handle: Handle = match arg {
            Some(entry) => process
                .handles
                .lock()
                .insert(entry.object, entry.rights, budget)?,
            None => HANDLE_INVALID,
        };
        process.state.store(RUNNING, Ordering::Release);
        process.started.fetch_add(1, Ordering::Relaxed);
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
        process.started.fetch_add(1, Ordering::Relaxed);
        Thread::start_user(thread, entry, stack, arg1, arg2);
        Ok(())
    }

    /// Makes `thread` DEAD. When it was the last started thread of its
    /// process that was not DEAD, the process terminates with exit code 0.
    pub fn kill_thread(thread: &Arc<Thread>) {
        let Some(process) = thread.process() else {
            return;
        };
        let threads = process.threads.lock();
        let was = sched::make_dead(thread);
        let last = (was == sched::RUNNABLE || was == sched::BLOCKED)
            && process.started.fetch_sub(1, Ordering::Relaxed) == 1;
        drop(threads);
        if last {
            process.terminate(0);
        }
    }

    /// Terminates the process with exit code `code`, unless it has
    /// terminated already: makes every thread DEAD, removes every user
    /// mapping, closes every handle, and sets the state `TERMINATED` and
    /// the `TERMINATED` signal on the process and its threads.
    pub fn terminate(&self, code: i64) {
        let threads: Vec<Arc<Thread>> = {
            let mut threads = self.threads.lock();
            if self.state() == TERMINATED {
                return;
            }
            threads.retain(|t| t.strong_count() > 0);
            let threads: Vec<_> = threads.iter().filter_map(Weak::upgrade).collect();
            for thread in &threads {
                sched::make_dead(thread);
            }
            self.exit_code.store(code, Ordering::Release);
            self.state.store(TERMINATED, Ordering::Release);
            threads
        };
        self.aspace.remove_regions(USER_START, USER_END);
        // Frames mapped outside regions belong to whoever mapped them, so
        // they are not freed.
        self.aspace
            .unmap(USER_START, USER_END - USER_START, |_| {})
            .expect("user mappings are 4 KiB pages inside the user range");
        // Objects are dropped after the lock is released, as dropping one
        // can drop further objects.
        let handles = core::mem::take(&mut *self.handles.lock());
        drop(handles);
        self.header.update_signals(0, signals::TERMINATED);
        for thread in &threads {
            thread.header().update_signals(0, signals::TERMINATED);
        }
    }
}

impl PeerObserver for Process {
    /// The handler's endpoint is gone: threads waiting in an exception
    /// wake up and terminate the process.
    fn peer_closed(&self) {
        let threads = self.threads.lock();
        for thread in threads.iter().filter_map(Weak::upgrade) {
            sched::resume_exception(&thread, sched::HANDLER_GONE);
        }
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
