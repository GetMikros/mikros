//! The state that serving init and every `Launcher` endpoint share, and
//! starting a process with its whole startup transfer.

use crate::broker::Permissions;
use crate::devfs::Dev;
use crate::launcher;
use crate::namespace::{App, Namespaces, Programs};
use crate::portable::Portable;
use crate::records::{Exit, Record, Records};
use crate::start::{self, Transfer, duplicate};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_bootfs::server::Fs;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeAsyncClient, Rights};
use mikros_rt::interfaces::mikros::process::{Launcher, NamespaceEntry, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, ProcessDirectoryAsyncClient,
};
use mikros_rt::sys;

const SAME_RIGHTS: u32 = 1 << 31;
const INFO_BASIC: u32 = 1;
const INFO_BASIC_SIZE: usize = 24;
const INFO_PROCESS: u32 = 2;
const INFO_PROCESS_SIZE: usize = 0x28;
/// The signals of a process that has terminated, is stopped, and was
/// continued.
const TERMINATED: u32 = 1 << 4;
const SUSPENDED: u32 = 1 << 5;
const CONTINUED: u32 = 1 << 7;
/// The process state of a stopped process.
const STATE_STOPPED: u32 = 3;
/// The signal a parent gets when a child terminates.
const CHLD: u32 = 17;

/// What the process manager's tasks share.
pub struct Shared {
    pub fs: Rc<Fs>,
    /// The `/dev` of every process.
    pub dev: Dev,
    /// `/volumes`, for the processes that hold `user.volumes.access`.
    pub volumes: Rc<crate::volumes::Volumes>,
    /// How the namespaces of new processes are built.
    pub namespaces: Rc<Namespaces>,
    /// The root budget.
    pub budget: OwnedHandle,
    pub clock_page: OwnedHandle,
    /// The process manager's own process, which the files of programs
    /// are mapped into while they are loaded.
    pub process: OwnedHandle,
    /// The time service's time page, once it gave it.
    pub time_page: RefCell<Option<OwnedHandle>>,
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
    /// What the permission broker says about permissions.
    pub permissions: Permissions,
    /// The keys of the folder grants (MIKSPEC-0050.R38).
    pub grants: crate::grants::Grants,
    /// How many times a process terminated, stopped or was continued.
    changes: Cell<u64>,
    /// The tasks waiting for a process to terminate, stop or be
    /// continued.
    waiters: RefCell<Vec<Waker>>,
    /// The login sessions sent `KILL`, whose processes may start no more.
    pub killed_sessions: RefCell<BTreeSet<u32>>,
    /// The commands, and whether they were read from the root file
    /// system's `/system`.
    pub commands: RefCell<Option<(bool, Rc<crate::commands::Commands>)>>,
    /// The log service's `ProcessSources`, which the standard output and
    /// error that processes are given go to (MIKSPEC-0063.R21).
    pub output: crate::output::Sink,
}

/// A process to start.
pub struct NewProcess {
    /// The absolute path of the program, which the record keeps.
    pub path: String,
    /// The namespace that `path` is resolved in: the spawner's.
    pub programs: Rc<Programs>,
    /// Where `path`, when it is under `/system`, is looked for before
    /// `programs`: the root file system for the device manager's programs
    /// once namespaces come from it.
    pub programs_first: Option<Rc<Programs>>,
    pub parent: u32,
    pub process_group: u32,
    pub session: u32,
    pub app_id: String,
    /// The app ID whose namespace it gets: its own, or for a command, its
    /// spawner's.
    pub namespace_of: String,
    /// Whether it keeps its spawner's app ID, which for an installed app
    /// leaves its folder and home as its spawner gives them.
    pub inherits_app: bool,
    pub user: u32,
    pub login_session: u32,
    /// The administrator who elevated its spawner, 0 for none.
    pub admin: u32,
    pub arguments: OwnedHandle,
    pub handles: Vec<StartupHandle>,
    /// Whether it is the service manager that init asked for.
    pub service_manager: bool,
    /// Whether the service manager starts it, which may start the
    /// service of a global package as user 0 (MIKSPEC-0050.R52).
    pub by_service_manager: bool,
    /// The folder it starts in, as its spawner named it, or `/`.
    pub working_directory: String,
    /// For a command of a package, the folder of its spawner's that holds
    /// that folder, at its real path, and the rights it has there, which
    /// it gets in its namespace and starts in (MIKSPEC-0101.R44).
    pub working_entry: Option<(String, Rights)>,
    /// How it runs when it is a portable app or a build, which its
    /// processes that keep its app ID share (MIKSPEC-0101.R46,
    /// MIKSPEC-0101.R47).
    pub portable: Option<Rc<Portable>>,
    /// Its file creation mask, as its spawner named it, or 0o022 for the
    /// default.
    pub umask: u32,
}

impl Shared {
    pub fn new(
        fs: Rc<Fs>,
        namespaces: Rc<Namespaces>,
        budget: OwnedHandle,
        clock_page: OwnedHandle,
        process: OwnedHandle,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Rc<Shared> {
        Rc::new(Shared {
            fs,
            process,
            dev: Dev::new(reactor.clone(), spawner.clone()),
            volumes: crate::volumes::Volumes::new(reactor.clone(), spawner.clone()),
            namespaces,
            budget,
            clock_page,
            time_page: RefCell::new(None),
            reactor,
            spawner,
            records: RefCell::new(Records::default()),
            user_budgets: RefCell::new(BTreeMap::new()),
            app_budgets: RefCell::new(BTreeMap::new()),
            control: RefCell::new(None),
            identity_setter: Cell::new(None),
            permissions: Permissions::default(),
            grants: Default::default(),
            changes: Cell::new(0),
            waiters: RefCell::new(Vec::new()),
            killed_sessions: RefCell::new(BTreeSet::new()),
            commands: RefCell::new(None),
            output: Default::default(),
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

    /// The entries of `namespace` bound with `Restrict` to the whole
    /// identity of the process of `identity`, with the number of its app
    /// ID (MIKSPEC-0050.R53), once namespaces come from the root file
    /// system: before, they come from the process manager alone, and the
    /// broker gives numbers only once it has read the stored ones there.
    /// Fails with the status of the first entry that cannot be bound, so
    /// that no entry is ever given unbound or bound to another process.
    async fn restricted(
        &self,
        namespace: Vec<NamespaceEntry>,
        identity: &ClientIdentity,
    ) -> Result<Vec<NamespaceEntry>, Status> {
        if !self.namespaces.on_root() {
            return Ok(namespace);
        }
        let app = self.permissions.app_number(&identity.app_id).await;
        let mut restricted = Vec::with_capacity(namespace.len());
        for entry in namespace {
            let NamespaceEntry { path, directory } = entry;
            let directory = NodeAsyncClient::new(directory, self.reactor.clone());
            let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
            let made = directory
                .restrict(
                    identity.user,
                    app,
                    identity.pid,
                    identity.koid,
                    identity.admin,
                    identity.portable_checksum.clone(),
                    server,
                )
                .await;
            match made {
                Ok(Ok(_)) => restricted.push(NamespaceEntry {
                    path,
                    directory: client,
                }),
                failed => {
                    mikros_rt::log!(
                        Warning,
                        "process-manager: binding {path} of pid {} failed: {failed:?}",
                        identity.pid
                    );
                    return Err(match failed {
                        Ok(Err(e)) => crate::namespace::status(e),
                        _ => Status::Io,
                    });
                }
            }
        }
        Ok(restricted)
    }

    /// Starts `new` as process `pid`, whose record it adds, or replaces
    /// when `replacing`. Its namespace is built first, so that a process
    /// that may not start fails as such whatever its program.
    async fn start_as(
        self: &Rc<Self>,
        pid: u32,
        new: NewProcess,
        control: &ProcessDirectoryAsyncClient,
        replacing: bool,
    ) -> Result<OwnedHandle, Status> {
        let name = new.path.rsplit('/').next().unwrap_or("process");
        let budget = self.budget_of(new.user, &new.app_id)?;
        let namespaces = self.namespaces.clone();
        let (mut namespace, mut programs, app) = namespaces
            .of(
                self,
                &new.namespace_of,
                new.user,
                new.admin,
                new.service_manager,
                new.by_service_manager.then_some(new.path.as_str()),
                new.portable.as_deref(),
            )
            .await?;
        // A command's folder of its spawner, unless the namespace has an
        // entry at that path already.
        let mut in_folder = false;
        if let Some((folder, rights)) = &new.working_entry {
            if !namespace.iter().any(|e| &e.path == folder) {
                let (entry, origin) = namespaces.folder(self, folder, *rights).await?;
                programs.add(entry.path.clone(), origin);
                namespace.push(entry);
            }
            in_folder = true;
        }
        // A portable app's program is found in its package's files first,
        // and an installed app's in its own namespace, such as a package's
        // service that the service manager starts (MIKSPEC-0101.R55).
        let from_package =
            new.portable.as_ref().is_some_and(|p| p.package.is_some()) || app.is_some();
        let lookup = start::Lookup {
            first: match from_package {
                true => Some(&programs),
                false => new.programs_first.as_deref(),
            },
            then: &new.programs,
            everywhere: from_package,
        };
        let (started, interpreted) = start::load(
            &self.fs,
            lookup,
            self.process.raw(),
            &self.reactor,
            budget.raw(),
            &new.path,
            name,
        )
        .await?;
        // The path of the executable, and of its package, in the new
        // namespace, from where its file comes from (MIKSPEC-0034.R46).
        let executable = interpreted
            .as_ref()
            .and_then(|before| before.first())
            .unwrap_or(&new.path);
        let (program, bundle) = placed(&new.programs, executable, app.as_ref());
        let NewProcess {
            path,
            parent,
            process_group,
            session,
            app_id,
            user,
            login_session,
            admin,
            arguments,
            mut handles,
            mut working_directory,
            umask,
            inherits_app,
            portable,
            ..
        } = new;
        let process_koid = koid(&started.process).inspect_err(|_| started.kill())?;
        // A script's interpreter gets its own path, the line's argument
        // and the script's path before the arguments after the first
        // (MIKSPEC-0034.R44).
        let arguments = match interpreted {
            Some(before) => start::with_interpreter(arguments, &before, &path)
                .inspect_err(|_| started.kill())?,
            None => arguments,
        };
        // A process with `/tmp` uses its own entry (MIKSPEC-0050.R43).
        let arguments = match namespace.iter().any(|e| e.path == "/tmp") {
            true => start::with_tmpdir(arguments).inspect_err(|_| started.kill())?,
            false => arguments,
        };
        // An installed app starts in its folder, which is its home, and
        // learns how it is installed (MIKSPEC-0101.R7, MIKSPEC-0101.R9). A
        // process that keeps its spawner's app ID learns only the latter
        // (MIKSPEC-0101.R41).
        let arguments = match &app {
            Some(app) => {
                let mut variables = alloc::vec![
                    ("MIKA_MODE", app.mode),
                    ("MIKA_EXECUTABLE", app.executable.as_str()),
                ];
                if !inherits_app {
                    variables.push(("HOME", app.folder.as_str()));
                    if let Some(cache) = &app.cache {
                        variables.push(("XDG_CACHE_HOME", cache.as_str()));
                    }
                    if !in_folder {
                        working_directory = app.folder.clone();
                    }
                }
                start::with_variables(arguments, &variables).inspect_err(|_| started.kill())?
            }
            None => arguments,
        };
        let identity = ClientIdentity {
            pid,
            user,
            app_id: app_id.clone(),
            admin,
            koid: process_koid,
            portable_checksum: portable
                .as_ref()
                .map(|p| p.checksum.clone())
                .unwrap_or_default(),
        };
        let namespace = self
            .restricted(namespace, &identity)
            .await
            .inspect_err(|_| started.kill())?;
        // Standard output and error for a process started without them.
        let kept = crate::output::give(&mut handles, &app_id);
        let forwarded = app_id.clone();
        let transferred = self
            .transfer(
                &started,
                identity,
                parent,
                arguments,
                handles,
                namespace,
                (working_directory, program, bundle),
                umask,
                control,
            )
            .await;
        let recorded = transferred.and_then(|()| {
            let record = Record {
                koid: process_koid,
                parent,
                process_group,
                session,
                app_id,
                user,
                login_session,
                admin,
                rebound: None,
                budget,
                path,
                programs: Rc::new(programs),
                portable,
                process: duplicate(started.process.raw(), SAME_RIGHTS)?,
                exit: None,
                stopped: None,
                continued: false,
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
                let forward = crate::output::forward(self.clone(), pid, forwarded, kept);
                self.spawner.spawn(forward);
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
    pub fn budget_of(&self, user: u32, app_id: &str) -> Result<OwnedHandle, Status> {
        let mut apps = self.app_budgets.borrow_mut();
        let key = (user, String::from(app_id));
        if let Some(budget) = apps.get(&key) {
            return duplicate(budget.raw(), SAME_RIGHTS);
        }
        let user_budget = self.user_budget(user)?;
        let budget = create_budget(user_budget.raw())?;
        let handle = duplicate(budget.raw(), SAME_RIGHTS)?;
        apps.insert(key, budget);
        Ok(handle)
    }

    /// A handle to the budget of `user`, which is created with no limits
    /// on first use, under the root budget.
    pub fn user_budget(&self, user: u32) -> Result<OwnedHandle, Status> {
        let mut users = self.user_budgets.borrow_mut();
        if let Some(budget) = users.get(&user) {
            return duplicate(budget.raw(), SAME_RIGHTS);
        }
        let budget = create_budget(self.budget.raw())?;
        let handle = duplicate(budget.raw(), SAME_RIGHTS)?;
        users.insert(user, budget);
        Ok(handle)
    }

    #[allow(clippy::too_many_arguments)]
    async fn transfer(
        self: &Rc<Self>,
        started: &start::Started,
        identity: ClientIdentity,
        parent: u32,
        arguments: OwnedHandle,
        handles: Vec<StartupHandle>,
        namespace: Vec<NamespaceEntry>,
        (working_directory, program, bundle): (String, String, Option<String>),
        umask: u32,
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
            time_page: self.time_page.borrow().as_ref().map(|page| page.raw()),
            namespace,
            working_directory,
            umask,
            program,
            bundle,
        })?;
        bind(control, identity, directory_end).await?;
        started.complete()
    }

    /// Watches process `pid` until it terminates, then keeps its exit
    /// status and wakes the tasks waiting for a change, unless another
    /// process replaced it. Until then, each time the process stopped or
    /// was continued, which its stop and continue counts tell even when
    /// both happened between two looks, the change is kept for its parent
    /// to collect, the parent gets CHLD, and the waiting tasks are woken.
    async fn watch(self: Rc<Self>, pid: u32, process: OwnedHandle) {
        let Ok(watched) = koid(&process) else {
            return;
        };
        let (mut stops, mut continues) = (0, 0);
        // The signal awaited besides TERMINATED is the one not set.
        let mut next = SUSPENDED;
        loop {
            let Ok(observed) = self.reactor.signals(process.raw(), TERMINATED | next).await else {
                return;
            };
            if observed & TERMINATED != 0 {
                break;
            }
            let Ok(record) = info::<INFO_PROCESS_SIZE>(&process, INFO_PROCESS) else {
                return;
            };
            let word =
                |at: usize| u32::from_le_bytes(record[at..at + 4].try_into().expect("4 bytes"));
            let stopped = word(0x08) == STATE_STOPPED;
            next = if stopped { CONTINUED } else { SUSPENDED };
            if (word(0x20), word(0x24)) == (stops, continues) {
                continue;
            }
            (stops, continues) = (word(0x20), word(0x24));
            let mut records = self.records.borrow_mut();
            let Some(record) = records.get_mut(pid).filter(|r| r.koid == watched) else {
                return;
            };
            // Only the latest change is reported: a stop that a continue
            // followed is not, nor a continue that a stop followed.
            record.stopped = stopped.then(|| word(0x14));
            record.continued = !stopped;
            let parent = record.parent;
            if let Some(parent) = records.get(parent).filter(|r| r.exit.is_none()) {
                // SAFETY: the call takes no pointers.
                unsafe { sys::process_signal(parent.process.raw(), CHLD, 0) };
            }
            drop(records);
            self.changed();
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
        self.changed();
    }

    /// Wakes the tasks waiting for a process to terminate, stop or be
    /// continued.
    fn changed(&self) {
        self.changes.set(self.changes.get() + 1);
        for waker in self.waiters.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    /// Completes once another process terminated, stopped or was
    /// continued, or `deadline` passed, which it returns as `false`.
    pub async fn change(&self, deadline: i64) -> bool {
        let seen = self.changes.get();
        let timer = self.reactor.sleep_until(deadline);
        let mut timer = core::pin::pin!(timer);
        poll_fn(|cx| {
            if self.changes.get() != seen {
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

/// Where the executable at the absolute `path`, which `programs` resolve,
/// lies in the namespace of a process that runs it as `app`, and the
/// folder of its package there when it is a program of that installed
/// app: a file of the package is seen in `/apps/<id>/package`, and
/// another path is kept as it is, normalized.
fn placed(programs: &Programs, path: &str, app: Option<&App>) -> (String, Option<String>) {
    let normal = mikros_manifest::path::normalize(path).unwrap_or_else(|| String::from(path));
    let Some(app) = app else {
        return (normal, None);
    };
    if let Some(seen) = programs.real_path(&normal).and_then(|real| app.sees(&real)) {
        return (seen, app.package.clone());
    }
    // Through the app's own view of its folder, or a portable app's
    // package.
    let below = |at: &str| {
        normal
            .strip_prefix(at)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    let mut roots = app.package.iter().map(String::as_str);
    let inside = (app.installed.is_some() && below(&app.folder)) || roots.any(below);
    let bundle = inside.then(|| app.package.clone()).flatten();
    (normal, bundle)
}
