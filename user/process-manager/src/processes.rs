//! The state that serving init and every `Launcher` endpoint share, and
//! starting a process with its whole startup transfer.

use crate::launcher;
use crate::records::{Exit, Record, Records};
use crate::start::{self, Transfer, duplicate};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{Launcher, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, ProcessDirectoryAsyncClient,
};
use mikros_rt::sys;
use process_manager::bootfs::Fs;
use process_manager::devfs::Dev;

const SAME_RIGHTS: u32 = 1 << 31;
const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
const INFO_PROCESS: u32 = 2;
const INFO_PROCESS_SIZE: usize = 32;
/// The signal of a process that has terminated.
const TERMINATED: u32 = 1 << 4;
/// The signal a parent gets when a child terminates.
const CHLD: u32 = 17;

/// What the process manager's tasks share.
pub struct Shared {
    pub fs: Rc<Fs>,
    /// The `/dev` of every process.
    dev: Dev,
    /// The root budget.
    pub budget: OwnedHandle,
    pub clock_page: OwnedHandle,
    pub reactor: Reactor,
    pub spawner: Spawner,
    pub records: RefCell<Records>,
    /// The budget of each user, under the root budget.
    user_budgets: RefCell<BTreeMap<u32, OwnedHandle>>,
    /// The budget of each user and app ID, under the user's budget.
    app_budgets: RefCell<BTreeMap<(u32, String), OwnedHandle>>,
    /// The service manager's control endpoint, once it started.
    pub control: RefCell<Option<Rc<ProcessDirectoryAsyncClient>>>,
    /// The process ID of the service manager that init asked for, the
    /// one record allowed to set identities until the permission broker
    /// serves requests.
    pub identity_setter: Cell<Option<u32>>,
    /// How many processes terminated so far.
    terminations: Cell<u64>,
    /// The tasks waiting for a process to terminate.
    waiters: RefCell<Vec<Waker>>,
}

/// A process to start.
pub struct NewProcess {
    /// The program, relative to the root of the boot file system.
    pub program: String,
    /// The executable path the record keeps.
    pub path: String,
    pub parent: u32,
    pub process_group: u32,
    pub session: u32,
    pub app_id: String,
    pub user: u32,
    pub login_session: u32,
    pub arguments: OwnedHandle,
    pub handles: Vec<StartupHandle>,
}

impl Shared {
    pub fn new(
        fs: Rc<Fs>,
        budget: OwnedHandle,
        clock_page: OwnedHandle,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Rc<Shared> {
        Rc::new(Shared {
            fs,
            dev: Dev::new(reactor.clone(), spawner.clone()),
            budget,
            clock_page,
            reactor,
            spawner,
            records: RefCell::new(Records::default()),
            user_budgets: RefCell::new(BTreeMap::new()),
            app_budgets: RefCell::new(BTreeMap::new()),
            control: RefCell::new(None),
            identity_setter: Cell::new(None),
            terminations: Cell::new(0),
            waiters: RefCell::new(Vec::new()),
        })
    }

    /// Starts `new` as process `pid`, which the caller reserved: loads it, sends its startup
    /// transfer, binds its directory over `control` and only then sends
    /// `Complete`. Records it and watches for its end. Returns its
    /// process handle. A failure after the process was created kills it,
    /// and any failure releases `pid`.
    pub async fn start(
        self: &Rc<Self>,
        pid: u32,
        new: NewProcess,
        control: &ProcessDirectoryAsyncClient,
    ) -> Result<OwnedHandle, Status> {
        let started = self.start_as(pid, new, control, false).await;
        if started.is_err() {
            self.records.borrow_mut().release(pid);
        }
        started
    }

    /// Starts `new` in place of the running process `pid`, as `start`
    /// does, then puts its record in place of that process's and kills
    /// that process, whose end is not reported.
    pub async fn replace(
        self: &Rc<Self>,
        pid: u32,
        new: NewProcess,
        control: &ProcessDirectoryAsyncClient,
    ) -> Result<(), Status> {
        self.start_as(pid, new, control, true).await.map(drop)
    }

    /// Starts `new` as process `pid`, whose record it adds, or replaces
    /// when `replacing`.
    async fn start_as(
        self: &Rc<Self>,
        pid: u32,
        new: NewProcess,
        control: &ProcessDirectoryAsyncClient,
        replacing: bool,
    ) -> Result<OwnedHandle, Status> {
        let name = new.program.rsplit('/').next().unwrap_or("process");
        let budget = self.budget_of(new.user, &new.app_id)?;
        let started = start::load(&self.fs, budget.raw(), &new.program, name)?;
        let NewProcess {
            path,
            parent,
            process_group,
            session,
            app_id,
            user,
            login_session,
            arguments,
            handles,
            ..
        } = new;
        let identity = ClientIdentity {
            pid,
            user,
            app_id: app_id.clone(),
        };
        let transferred = self
            .transfer(&started, identity, parent, arguments, handles, control)
            .await;
        let recorded = transferred.and_then(|()| {
            let record = Record {
                koid: koid(&started.process)?,
                parent,
                process_group,
                session,
                app_id,
                user,
                login_session,
                budget,
                path,
                process: duplicate(started.process.raw(), SAME_RIGHTS)?,
                exit: None,
            };
            let watched = duplicate(started.process.raw(), SAME_RIGHTS)?;
            let mut records = self.records.borrow_mut();
            if replacing {
                let old = records.replace(pid, record).ok_or(Status::BadState)?;
                // SAFETY: the call takes no pointers.
                unsafe { sys::task_kill(old.process.raw()) };
            } else if !records.insert(pid, record) {
                return Err(Status::BadState);
            }
            Ok(watched)
        });
        match recorded {
            Ok(watched) => {
                self.spawner.spawn(self.clone().watch(pid, watched));
                Ok(started.process)
            }
            Err(status) => {
                started.kill();
                Err(status)
            }
        }
    }

    /// A handle to the budget of `user` and `app_id`, which is created
    /// with no limits on first use, like the user's budget above it.
    fn budget_of(&self, user: u32, app_id: &str) -> Result<OwnedHandle, Status> {
        let mut apps = self.app_budgets.borrow_mut();
        let key = (user, String::from(app_id));
        if let Some(budget) = apps.get(&key) {
            return duplicate(budget.raw(), SAME_RIGHTS);
        }
        let mut users = self.user_budgets.borrow_mut();
        let user_budget = match users.get(&user) {
            Some(budget) => budget.raw(),
            None => {
                let budget = create_budget(self.budget.raw())?;
                let raw = budget.raw();
                users.insert(user, budget);
                raw
            }
        };
        let budget = create_budget(user_budget)?;
        let handle = duplicate(budget.raw(), SAME_RIGHTS)?;
        apps.insert(key, budget);
        Ok(handle)
    }

    async fn transfer(
        self: &Rc<Self>,
        started: &start::Started,
        identity: ClientIdentity,
        parent: u32,
        arguments: OwnedHandle,
        handles: Vec<StartupHandle>,
        control: &ProcessDirectoryAsyncClient,
    ) -> Result<(), Status> {
        let channel = |_| Status::NoResources;
        let (launcher, launcher_end) = endpoints::<Launcher>().map_err(channel)?;
        let (directory, directory_end) = endpoints::<Directory>().map_err(channel)?;
        self.spawner
            .spawn(launcher::serve(launcher_end, identity.pid, self.clone()));
        started.send(Transfer {
            pid: identity.pid,
            parent,
            user: identity.user,
            app_id: identity.app_id.clone(),
            arguments,
            handles,
            launcher,
            directory,
            clock_page: self.clock_page.raw(),
            root: start::root(&self.fs)?,
            dev: self.dev.directory()?,
        })?;
        bind(control, identity, directory_end).await?;
        started.complete()
    }

    /// Waits for process `pid` to terminate, then keeps its exit status
    /// and wakes the tasks waiting for a change, unless another process
    /// replaced it.
    async fn watch(self: Rc<Self>, pid: u32, process: OwnedHandle) {
        let Ok(watched) = koid(&process) else {
            return;
        };
        if self
            .reactor
            .signals(process.raw(), TERMINATED)
            .await
            .is_err()
        {
            return;
        }
        let exit = exit_of(&process).unwrap_or(Exit { code: 0, signal: 0 });
        let mut records = self.records.borrow_mut();
        let Some(parent) = records
            .get(pid)
            .filter(|r| r.koid == watched)
            .map(|r| r.parent)
        else {
            return;
        };
        records.exited(pid, exit);
        // The parent learns of the end with CHLD, unless it ended too.
        if let Some(parent) = records.get(parent).filter(|r| r.exit.is_none()) {
            // SAFETY: the call takes no pointers.
            unsafe { sys::process_signal(parent.process.raw(), CHLD, 0) };
        }
        drop(records);
        self.terminations.set(self.terminations.get() + 1);
        for waker in self.waiters.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// Completes once another process terminated, or `deadline` passed,
    /// which it returns as `false`.
    pub async fn termination(&self, deadline: i64) -> bool {
        let seen = self.terminations.get();
        let timer = self.reactor.sleep_until(deadline);
        let mut timer = core::pin::pin!(timer);
        poll_fn(|cx| {
            if self.terminations.get() != seen {
                return Poll::Ready(true);
            }
            if timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(false);
            }
            self.waiters.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

/// Binds `directory` to `identity` over the service manager's control
/// endpoint, and waits for the acknowledgment.
pub async fn bind(
    control: &ProcessDirectoryAsyncClient,
    identity: ClientIdentity,
    directory: ServerEnd<Directory>,
) -> Result<(), Status> {
    match control.bind(identity, directory).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// A new budget under `parent` with no limits.
fn create_budget(parent: sys::Handle) -> Result<OwnedHandle, Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::budget_create(parent, u64::MAX, u64::MAX, &mut out) };
    match status {
        // SAFETY: the kernel just created the handle for us.
        mikros_rt::OK => Ok(unsafe { OwnedHandle::from_raw(out) }),
        status => Err(start::status(status)),
    }
}

fn info<const N: usize>(handle: &OwnedHandle, topic: u32) -> Result<[u8; N], Status> {
    let mut record = [0u8; N];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(handle.raw(), topic, record.as_mut_ptr(), N as u64, &mut len)
    };
    match status {
        mikros_rt::OK => Ok(record),
        status => Err(start::status(status)),
    }
}

/// The kernel object ID of `handle`'s object.
pub fn koid(handle: &OwnedHandle) -> Result<u64, Status> {
    let record = info::<INFO_BASIC_SIZE>(handle, INFO_BASIC)?;
    Ok(u64::from_le_bytes(record[..8].try_into().expect("8 bytes")))
}

/// The exit code and terminating signal of the terminated `process`.
fn exit_of(process: &OwnedHandle) -> Result<Exit, Status> {
    let record = info::<INFO_PROCESS_SIZE>(process, INFO_PROCESS)?;
    Ok(Exit {
        code: i64::from_le_bytes(record[..8].try_into().expect("8 bytes")),
        signal: u32::from_le_bytes(record[0x10..0x14].try_into().expect("4 bytes")),
    })
}
