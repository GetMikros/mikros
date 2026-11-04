//! Processes: an address space, a handle table and threads.

use crate::aspace::AddressSpace;
use crate::sched::{self, Thread};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use kernel_core::exception::DEBUG_EVENTS;
use kernel_core::object::budget::{Budget, Held, Scratch};
use kernel_core::object::channel::{Endpoint, PeerObserver};
use kernel_core::object::handles::{Entry, HANDLE_INVALID, Handle, HandleTable};
use kernel_core::object::{Header, Name, Object, ObjectType, signals};
use kernel_core::status::Error;
use kernel_core::sync::SpinLock;

pub const NEW: u32 = 0;
pub const RUNNING: u32 = 1;
pub const TERMINATED: u32 = 2;
pub const STOPPED: u32 = 3;

/// The topic and size of the process information record.
pub const INFO_PROCESS: u32 = 2;
/// The topic of the list of a process's threads.
pub const INFO_THREADS: u32 = 7;
pub const INFO_PROCESS_SIZE: usize = 0x20;

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
    signals: SpinLock<crate::signal::ProcessSignals>,
    /// The signal that terminated the process, or 0.
    term_signal: AtomicU32,
    /// The threads waiting while the process is stopped.
    stopped: SpinLock<Vec<Arc<Thread>>>,
    /// The I/O port ranges, start and exclusive end, that the process's
    /// threads may access.
    ioports: SpinLock<Ioports>,
    /// The nanoseconds its threads have run, up to their last switch.
    cpu_time: AtomicI64,
    /// The policies of `process_set_policy`.
    policy: AtomicU32,
    /// The range of `process_set_syscall_gate`, start and exclusive end;
    /// an end of 0 means no gate.
    gate_start: AtomicU64,
    gate_end: AtomicU64,
    /// The neighbours in the list of all processes, or null.
    prev: AtomicPtr<Process>,
    next: AtomicPtr<Process>,
}

/// The first of all processes, newest first, linked through
/// `Process::next` and `Process::prev`, which the kernel debugger
/// scripts follow. The links change only under `REGISTRY`, and a
/// process leaves the list before its memory goes.
static FIRST_PROCESS: AtomicPtr<Process> = AtomicPtr::new(core::ptr::null_mut());
static REGISTRY: SpinLock<()> = SpinLock::new(());

/// Adds `process` to the list of all processes.
fn register(process: &Arc<Process>) {
    let _registry = REGISTRY.lock();
    let this = Arc::as_ptr(process).cast_mut();
    let first = FIRST_PROCESS.load(Ordering::Relaxed);
    process.next.store(first, Ordering::Relaxed);
    if !first.is_null() {
        // SAFETY: listed processes are alive, and the lock is held.
        unsafe { &*first }.prev.store(this, Ordering::Relaxed);
    }
    FIRST_PROCESS.store(this, Ordering::Release);
}

/// Removes `process`, which is being dropped, from the list of all
/// processes, if it is listed.
fn unregister(process: &Process) {
    let _registry = REGISTRY.lock();
    let this = core::ptr::from_ref(process).cast_mut();
    let (prev, next) = (
        process.prev.load(Ordering::Relaxed),
        process.next.load(Ordering::Relaxed),
    );
    if prev.is_null() && FIRST_PROCESS.load(Ordering::Relaxed) != this {
        return;
    }
    // SAFETY: listed processes are alive, and the lock is held.
    unsafe {
        match prev.is_null() {
            true => FIRST_PROCESS.store(next, Ordering::Release),
            false => (*prev).next.store(next, Ordering::Relaxed),
        }
        if !next.is_null() {
            (*next).prev.store(prev, Ordering::Relaxed);
        }
    }
}

/// The policy bits of `process_set_policy`.
pub const ALLOW_WRITE_EXECUTE: u32 = 1 << 0;
pub const CODE_32BIT: u32 = 1 << 1;

/// The initial process, once it exists.
static INIT: SpinLock<Weak<Process>> = SpinLock::new(Weak::new());

/// Makes `process` the initial process, whose termination is fatal.
pub fn set_init(process: &Arc<Process>) {
    *INIT.lock() = Arc::downgrade(process);
}

/// Panics when `process` is the initial process. Called as a process
/// terminates.
fn terminated(process: &Process, code: i64) {
    if core::ptr::eq(INIT.lock().as_ptr(), process) {
        panic!("init terminated with exit code {code}");
    }
}

/// Fails with `InvalidArgs` unless `entry` lies in the user address range
/// and `stack` lies in it or is its end, the top of a stack that ends
/// there. Neither needs to be mapped.
fn check_start(entry: u64, stack: u64) -> Result<(), Error> {
    use crate::aspace::{in_user_range, user_range};
    let (start, end) = user_range();
    if !in_user_range(entry, 1) || !(start..=end).contains(&stack) {
        return Err(Error::InvalidArgs);
    }
    Ok(())
}

/// The I/O port ranges of a process, start and exclusive end, and the
/// charge for each.
#[derive(Default)]
struct Ioports {
    ranges: Vec<(u64, u64)>,
    charges: Vec<Held>,
}

/// The kernel memory charged for an I/O port range of a process: its
/// entries and their share of storage.
const IOPORT_COST: u64 = 2 * (size_of::<(u64, u64)>() + size_of::<Held>()) as u64;

/// Makes this processor's I/O permission bitmap allow exactly the ports
/// of `process`, or none for a kernel thread. Called as a thread is
/// switched in.
pub fn switch_ports(process: Option<&Process>) {
    let ports = process.map(|p| p.ioports.lock());
    kernel_core::arch::io_ports_switch(ports.as_deref().map_or(&[], |p| p.ranges.as_slice()));
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
        let aspace = match kernel_core::fallible::try_arc(aspace) {
            Ok(aspace) => aspace,
            Err(e) => {
                budget.release_kernel(charged);
                return Err(e);
            }
        };
        budget.add_process();
        // The process undoes both when it goes, also on failure.
        kernel_core::fallible::try_arc(Process {
            header: Header::new(ObjectType::Process),
            name,
            budget: budget.clone(),
            charged,
            aspace,
            handles: SpinLock::new(HandleTable::new()),
            state: AtomicU32::new(NEW),
            exit_code: AtomicI64::new(0),
            threads: SpinLock::new(Vec::new()),
            started: AtomicU32::new(0),
            exception: SpinLock::new(None),
            signals: SpinLock::new(crate::signal::ProcessSignals::default()),
            term_signal: AtomicU32::new(0),
            stopped: SpinLock::new(Vec::new()),
            ioports: SpinLock::new(Ioports::default()),
            cpu_time: AtomicI64::new(0),
            policy: AtomicU32::new(0),
            gate_start: AtomicU64::new(0),
            gate_end: AtomicU64::new(0),
            prev: AtomicPtr::new(core::ptr::null_mut()),
            next: AtomicPtr::new(core::ptr::null_mut()),
        })
        .inspect(register)
    }

    #[expect(dead_code, reason = "no call reads process names yet")]
    pub fn name(&self) -> &Name {
        &self.name
    }

    /// Lets the threads of the process access the ports from `start` to
    /// `end`, exclusive, until it terminates. `IOPORT_COST` is charged to
    /// `budget` until then. Fails with `NoMemory`, granting nothing,
    /// without room to record the range or to open it when the process's
    /// threads switch in.
    pub fn enable_ioports(&self, start: u64, end: u64, budget: &Arc<Budget>) -> Result<(), Error> {
        let charge = budget.hold_kernel(IOPORT_COST)?;
        let mut ports = self.ioports.lock();
        ports.ranges.try_reserve(1).map_err(|_| Error::NoMemory)?;
        ports.charges.try_reserve(1).map_err(|_| Error::NoMemory)?;
        if !kernel_core::arch::io_ports_reserve(ports.ranges.len() + 1) {
            return Err(Error::NoMemory);
        }
        ports.ranges.push((start, end));
        ports.charges.push(charge);
        drop(ports);
        let running_here = sched::current()
            .and_then(|t| t.process().cloned())
            .is_some_and(|p| core::ptr::eq(&*p, self));
        if running_here {
            switch_ports(Some(self));
        }
        Ok(())
    }

    /// Replaces the process's policies with `policy`, as
    /// `process_set_policy` does. Fails with `InvalidArgs` for undefined
    /// bits, with `NotSupported` for `CODE_32BIT`, which nothing runs yet,
    /// and with `BadState` unless the process is `NEW`.
    pub fn set_policy(&self, policy: u32) -> Result<(), Error> {
        if policy & !(ALLOW_WRITE_EXECUTE | CODE_32BIT) != 0 {
            return Err(Error::InvalidArgs);
        }
        if policy & CODE_32BIT != 0 {
            return Err(Error::NotSupported);
        }
        // The state cannot change meanwhile: starting takes this lock.
        let _threads = self.threads.lock();
        if self.state() != NEW {
            return Err(Error::BadState);
        }
        self.policy.store(policy, Ordering::Relaxed);
        Ok(())
    }

    /// Restricts system calls of the process to the `len` bytes of code
    /// from `start`, as `process_set_syscall_gate` does. Fails with
    /// `InvalidArgs` when the range is empty or does not fit in 64 bits,
    /// and with `BadState` unless the process is `NEW` without a gate.
    pub fn set_syscall_gate(&self, start: u64, len: u64) -> Result<(), Error> {
        let end = start.checked_add(len).filter(|_| len != 0);
        let end = end.ok_or(Error::InvalidArgs)?;
        // The state cannot change meanwhile: starting takes this lock.
        let _threads = self.threads.lock();
        if self.state() != NEW || self.gate_end.load(Ordering::Relaxed) != 0 {
            return Err(Error::BadState);
        }
        self.gate_start.store(start, Ordering::Relaxed);
        self.gate_end.store(end, Ordering::Relaxed);
        Ok(())
    }

    /// Whether a system call instruction at `at` is handled as one.
    pub fn syscall_allowed(&self, at: u64) -> bool {
        let end = self.gate_end.load(Ordering::Relaxed);
        end == 0 || (self.gate_start.load(Ordering::Relaxed)..end).contains(&at)
    }

    /// Whether mappings of the process may be writable and executable.
    pub fn allows_write_execute(&self) -> bool {
        self.policy.load(Ordering::Relaxed) & ALLOW_WRITE_EXECUTE != 0
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

    pub fn signals(&self) -> &SpinLock<crate::signal::ProcessSignals> {
        &self.signals
    }

    /// The threads of the process that still exist, listed in room
    /// charged to `scratch`, or `NoMemory` without room to list them.
    pub fn live_threads(&self, scratch: &mut Scratch) -> Result<Vec<Arc<Thread>>, Error> {
        let threads = self.threads.lock();
        let mut live = Vec::new();
        scratch.reserve_exact(&mut live, threads.len())?;
        live.extend(threads.iter().filter_map(Weak::upgrade));
        drop(threads);
        Ok(live)
    }

    /// The `THREADS` information record: the object IDs of the threads
    /// that are not `DEAD`, listed in room charged to `scratch`.
    pub fn thread_list(&self, scratch: &mut Scratch) -> Result<Vec<u8>, Error> {
        let live = self.live_threads(scratch)?;
        let mut list = Vec::new();
        scratch.reserve_exact(&mut list, live.len() * size_of::<u64>())?;
        for thread in live.iter().filter(|t| t.state() != sched::DEAD) {
            list.extend_from_slice(&thread.header().koid().to_le_bytes());
        }
        Ok(list)
    }

    /// The thread with object ID `koid` that is not `DEAD`, looked up in
    /// room charged to `scratch`.
    pub fn thread(&self, koid: u64, scratch: &mut Scratch) -> Result<Option<Arc<Thread>>, Error> {
        let live = self.live_threads(scratch)?;
        Ok(live
            .into_iter()
            .find(|t| t.header().koid() == koid && t.state() != sched::DEAD))
    }

    /// The signal that terminated the process, or 0.
    #[allow(dead_code, reason = "the PROCESS information topic comes later")]
    /// Adds `ns` nanoseconds that one of its threads ran.
    pub fn add_cpu_time(&self, ns: i64) {
        self.cpu_time.fetch_add(ns, Ordering::Relaxed);
    }

    /// The `PROCESS` information record. `thread_count` counts the
    /// threads that are not `DEAD`.
    pub fn info(&self) -> [u8; INFO_PROCESS_SIZE] {
        let threads = self
            .threads
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|t| t.state() != sched::DEAD)
            .count() as u32;
        let running =
            sched::running_slice(|t| t.process().is_some_and(|p| core::ptr::eq(&**p, self)));
        let cpu_time = self.cpu_time.load(Ordering::Relaxed) + running;
        let mut record = [0u8; INFO_PROCESS_SIZE];
        record[0x00..0x08].copy_from_slice(&self.exit_code().to_le_bytes());
        record[0x08..0x0C].copy_from_slice(&self.state().to_le_bytes());
        record[0x0C..0x10].copy_from_slice(&threads.to_le_bytes());
        record[0x10..0x14].copy_from_slice(&self.term_signal().to_le_bytes());
        record[0x18..0x20].copy_from_slice(&cpu_time.to_le_bytes());
        record
    }

    pub fn term_signal(&self) -> u32 {
        self.term_signal.load(Ordering::Acquire)
    }

    /// Terminates the process because of signal `signo`.
    pub fn terminate_by_signal(&self, signo: u32) {
        let _ = self
            .term_signal
            .compare_exchange(0, signo, Ordering::AcqRel, Ordering::Acquire);
        self.terminate(kernel_core::signal::EXIT_CODE);
    }

    /// Stops a running process: its threads wait at their next return to
    /// user mode.
    pub fn stop(&self) {
        if self
            .state
            .compare_exchange(RUNNING, STOPPED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.header.update_signals(0, signals::SUSPENDED);
        }
    }

    /// Makes the running `thread` of this process wait until the process
    /// runs again, is terminated, or is sent KILL.
    pub fn stop_here(&self, thread: &Arc<Thread>) {
        let mut stopped = self.stopped.lock();
        if self.state() != STOPPED || stopped.try_reserve(1).is_err() {
            return;
        }
        stopped.push(thread.clone());
        sched::block(thread, move || drop(stopped));
    }

    /// Wakes the threads waiting in a stopped process, so that they look
    /// at its signals again.
    pub fn wake_stopped(&self) {
        let threads = core::mem::take(&mut *self.stopped.lock());
        for thread in &threads {
            sched::wake(thread);
        }
    }

    /// Lets the threads of a stopped process run again.
    pub fn continue_threads(&self) {
        if self
            .state
            .compare_exchange(STOPPED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.header.update_signals(signals::SUSPENDED, 0);
        }
        self.wake_stopped();
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
    /// arguments. Fails with `InvalidArgs` as `check_start` does and
    /// with `BadState` unless the process is `NEW` and `thread` is one of
    /// its threads and `NEW`, in each case changing nothing. A new handle
    /// table slot for `arg` is charged to `budget`, if given.
    pub fn start(
        process: &Arc<Process>,
        thread: &Arc<Thread>,
        entry: u64,
        stack: u64,
        arg: Option<Entry>,
        arg2: u64,
        budget: Option<&Arc<Budget>>,
    ) -> Result<(), Error> {
        check_start(entry, stack)?;
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
    /// `InvalidArgs` as `check_start` does or with `BadState`.
    pub fn start_thread(
        thread: &Arc<Thread>,
        entry: u64,
        stack: u64,
        arg1: u64,
        arg2: u64,
    ) -> Result<(), Error> {
        check_start(entry, stack)?;
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
        // Every started state counts, a thread stopped in an exception
        // included.
        let started = was != sched::NEW && was != sched::DEAD;
        let last = started && process.started.fetch_sub(1, Ordering::Relaxed) == 1;
        drop(threads);
        if last {
            process.terminate(0);
        } else {
            thread.header().update_signals(0, signals::TERMINATED);
        }
    }

    /// Terminates the process with exit code `code`, unless it has
    /// terminated already: makes every thread DEAD, removes every user
    /// mapping, closes every handle, and sets the state `TERMINATED` and
    /// the `TERMINATED` signal on the process and its threads.
    pub fn terminate(&self, code: i64) {
        {
            let mut threads = self.threads.lock();
            if self.state() == TERMINATED {
                return;
            }
            threads.retain(|t| t.strong_count() > 0);
            for thread in threads.iter().filter_map(Weak::upgrade) {
                sched::make_dead(&thread);
            }
            self.exit_code.store(code, Ordering::Release);
            self.state.store(TERMINATED, Ordering::Release);
        }
        terminated(self, code);
        // This also removes translations made outside regions, whose
        // frames belong to whoever mapped them and are not freed.
        self.aspace.remove_all_regions();
        // Objects are dropped after the lock is released, as dropping one
        // can drop further objects.
        let mut handles = core::mem::take(&mut *self.handles.lock());
        while !handles.destroy_some(crate::publish::ITEM_STEP) {
            crate::sched::preempt_point_if_unlocked();
        }
        crate::wait::process_ended(self);
        drop(core::mem::take(&mut *self.stopped.lock()));
        self.header
            .update_signals(signals::SUSPENDED, signals::TERMINATED);
        // Once TERMINATED, the process gains no threads.
        for thread in self.threads.lock().iter().filter_map(Weak::upgrade) {
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
        unregister(self);
        self.budget.remove_process();
        self.budget.release_kernel(self.charged);
    }
}
