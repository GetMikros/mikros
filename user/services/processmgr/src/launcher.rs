//! The `Launcher` endpoint of each process the process manager starts.
//!
//! Each process has its own channel, so the record it is bound to is
//! the one it was created for, and requests act for that record only.
//! `Spawn`, `Exec`, `Wait`, `Kill`, `GetInfo`, `List`, `CreateSession`,
//! `SetProcessGroup` and `KillGroup` are served; every other method fails
//! with `NOT_SUPPORTED`.

use crate::broker::{FULL_DISK, LIST_ALL, SET_IDENTITY, SIGNAL_ANY};
use crate::processes::{NewProcess, Shared};
use crate::records::{Exit, Record, Records};
use crate::start::{self, duplicate};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    ExitStatus, Launcher, LauncherAsyncServer, LauncherCreateSessionResponse, LauncherExecResponse,
    LauncherGetInfoResponse, LauncherKillGroupResponse, LauncherKillResponse, LauncherListResponse,
    LauncherRequest, LauncherSetProcessGroupResponse, LauncherSpawnResponse, LauncherWaitResponse,
    ProcessInfo, SpawnRequest, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::sys;

/// Handle rights.
const SAME_RIGHTS: u32 = 1 << 31;
const TRANSFER: u32 = 1 << 1;
const WAIT: u32 = 1 << 6;
const SIGNAL: u32 = 1 << 7;
const MANAGE: u32 = 1 << 8;
const INSPECT: u32 = 1 << 9;
/// A `user` that means the caller's user ID.
const CALLER_USER: u32 = u32::MAX;
/// App IDs that need no bundle.
const SYSTEM_PREFIX: &str = "system.";
/// The app ID that `files.full_disk` lets a caller name, for programs
/// in `CLI_PROGRAMS`.
const CLI: &str = "system.cli";
const CLI_PROGRAMS: &str = "usr/bin";

/// Serves the `Launcher` endpoint `end` of process `pid` until it
/// closes. `Spawn`, `Exec` and `Wait` run as tasks of their own, so that one
/// that waits does not hold up the others.
pub async fn serve(end: ServerEnd<Launcher>, pid: u32, shared: Rc<Shared>) {
    use LauncherRequest as R;
    let server = Rc::new(LauncherAsyncServer::new(end, shared.reactor.clone()));
    let no = Status::NotSupported;
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            R::Spawn(request, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    let result = spawn(&shared, pid, request.request).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::Wait(request, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    let result = wait(
                        &shared,
                        pid,
                        request.selector,
                        request.options,
                        request.deadline,
                    )
                    .await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::Kill(request, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    let result = kill(&shared, pid, request.pid, request.signo).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::Exec(request, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    // On success the caller was killed, and the reply
                    // finds no one.
                    let result = exec(&shared, pid, request.request).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::GetInfo(request, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    let result = get_info(&shared, pid, request.pid).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::CreateSession(_, r) => r.reply(s, create_session(&shared, pid)),
            R::SetProcessGroup(request, r) => r.reply(
                s,
                set_process_group(&shared, pid, request.pid, request.process_group),
            ),
            R::KillGroup(request, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    let (group, signo) = (request.process_group, request.signo);
                    let result = kill_group(&shared, pid, group, signo).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::Fork(_, r) => r.reply(s, Err(no)),
            R::KillLoginSession(_, r) => r.reply(s, Err(no)),
            R::List(_, r) => {
                let (shared, server) = (shared.clone(), server.clone());
                shared.clone().spawner.spawn(async move {
                    let result = list(&shared, pid).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
        };
    }
}

/// The process that `request` of process `caller` starts with the
/// parent `parent`, and whether it keeps the caller's user and app ID.
async fn new_process(
    shared: &Shared,
    caller: u32,
    parent: impl FnOnce(&Record) -> u32,
    request: SpawnRequest,
) -> Result<(NewProcess, bool), Status> {
    // The path is resolved in the caller's namespace when the process is
    // loaded; binding an app ID looks at where it lies.
    let program = request.path.strip_prefix('/').ok_or(Status::InvalidArgs)?;
    // Without the permission broker no caller may name a user or start
    // a login session.
    if request.user != CALLER_USER || request.login_session {
        return Err(Status::AccessDenied);
    }
    let named = match &request.app_id {
        None => None,
        Some(app_id) => Some(named_app(shared, caller, app_id, program).await?),
    };
    let records = shared.records.borrow();
    let from = records.get(caller).ok_or(Status::BadState)?;
    let app_id = named.unwrap_or_else(|| from.app_id.clone());
    let same = app_id == from.app_id;
    let new = NewProcess {
        path: request.path.clone(),
        programs: from.programs.clone(),
        parent: parent(from),
        process_group: from.process_group,
        session: from.session,
        app_id,
        user: from.user,
        login_session: from.login_session,
        arguments: duplicate(request.arguments.raw(), SAME_RIGHTS)?,
        handles: request.handles,
        service_manager: false,
    };
    Ok((new, same))
}

/// Starts the program of `request` as a child of process `caller`.
async fn spawn(
    shared: &Rc<Shared>,
    caller: u32,
    request: SpawnRequest,
) -> Result<LauncherSpawnResponse, Status> {
    let (new_app, same) = new_process(shared, caller, |_| caller, request).await?;
    let control = shared.control.borrow().clone().ok_or(Status::BadState)?;
    let pid = shared
        .records
        .borrow_mut()
        .reserve()
        .ok_or(Status::NoResources)?;
    let process = shared.start(pid, new_app, &control).await?;
    let manage = if same { MANAGE } else { 0 };
    // `TRANSFER` and `INSPECT` let the handle travel in the reply and
    // be checked by the caller's decoder.
    let rights = WAIT | INSPECT | SIGNAL | TRANSFER | manage;
    Ok(LauncherSpawnResponse {
        pid,
        process: duplicate(process.raw(), rights)?,
    })
}

/// Replaces process `caller` with the program of `request`, which keeps
/// its process ID, parent and children.
async fn exec(
    shared: &Rc<Shared>,
    caller: u32,
    request: SpawnRequest,
) -> Result<LauncherExecResponse, Status> {
    let (new, _) = new_process(shared, caller, |from| from.parent, request).await?;
    let control = shared.control.borrow().clone().ok_or(Status::BadState)?;
    shared.replace(caller, new, &control).await?;
    Ok(LauncherExecResponse {})
}

/// Checks that process `caller` may give the new process of `program`, a
/// path relative to the boot file system's root, `app_id`, and returns
/// it. A caller with `files.full_disk` may name `system.cli` for a
/// program under `/usr/bin`. Any other app ID needs
/// `process.set_identity`, and one outside `system.` needs the program
/// to lie in the app's bundle under `/Apps`. A `system.` app ID with a
/// program manifest needs a manifest that is valid, the program to be
/// its `binary`, and the caller's app ID in its `spawned_by`; one whose
/// manifest cannot be used is refused. A program manifest of a service's
/// app ID is not valid, and counts as none.
async fn named_app(
    shared: &Shared,
    caller: u32,
    app_id: &str,
    program: &str,
) -> Result<String, Status> {
    let client = {
        let records = shared.records.borrow();
        let from = records.get(caller).ok_or(Status::BadState)?;
        ClientIdentity {
            pid: caller,
            user: from.user,
            app_id: from.app_id.clone(),
        }
    };
    let bootstrap = shared.identity_setter.get() == Some(caller);
    let holds = |permission| {
        shared
            .permissions
            .holds(clone(&client), bootstrap, permission)
    };
    if app_id == CLI && shared.fs.lies_in(program, CLI_PROGRAMS) && holds(FULL_DISK).await {
        return Ok(app_id.into());
    }
    if !holds(SET_IDENTITY).await {
        return Err(Status::AccessDenied);
    }
    let Some(name) = app_id.strip_prefix(SYSTEM_PREFIX) else {
        // User bundles are not in the boot file system.
        let bundle = format!("Apps/{app_id}");
        return match !app_id.contains('/') && shared.fs.lies_in(program, &bundle) {
            true => Ok(app_id.into()),
            false => Err(Status::AccessDenied),
        };
    };
    let file_name = format!("{app_id}.yaml");
    let path = format!("usr/lib/programs/{file_name}");
    let Some(bytes) = shared.fs.file(&path) else {
        return Ok(app_id.into());
    };
    // The app ID is a service's when its service manifest exists, and a
    // program manifest of it is then not valid, as none.
    if shared.fs.exists(&format!("usr/lib/services/{name}.yaml")) {
        mikros_rt::log!(
            Error,
            "/{path}: {app_id} is a service's app ID; the program manifest is ignored"
        );
        return Ok(app_id.into());
    }
    // A manifest that is not valid counts as none, as the service
    // manager ignores such service manifests.
    let manifest = match mikros_manifest::parse_program(&file_name, bytes) {
        Ok(manifest) => manifest,
        Err(e) => {
            mikros_rt::log!(Error, "/{path}:{}:{}: {:?}", e.line, e.column, e.kind);
            return Ok(app_id.into());
        }
    };
    let program_manifest = manifest.program;
    let binary = program_manifest.binary.trim_start_matches('/');
    let allowed = shared.fs.same_file(program, binary)
        && program_manifest.spawned_by.contains(&client.app_id);
    match allowed {
        true => Ok(app_id.into()),
        false => Err(Status::AccessDenied),
    }
}

/// Waits until a child of `caller` that `selector` chooses terminated,
/// or stopped or was continued when `options` asks for those, or until
/// `deadline` passed. A termination is collected; a stop or continuation
/// is reported once.
async fn wait(
    shared: &Shared,
    caller: u32,
    selector: i32,
    options: WaitOptions,
    deadline: i64,
) -> Result<LauncherWaitResponse, Status> {
    let stops = options.contains(WaitOptions::STOPPED);
    let continues = options.contains(WaitOptions::CONTINUED);
    loop {
        let found = {
            let records = shared.records.borrow();
            let mut children = records.children(caller, selector).peekable();
            if children.peek().is_none() {
                return Err(Status::NotFound);
            }
            children
                .find(|(_, r)| {
                    r.exit.is_some() || (stops && r.stopped.is_some()) || (continues && r.continued)
                })
                .map(|(pid, _)| pid)
        };
        if let Some(pid) = found {
            return report(shared, pid, stops);
        }
        if !shared.change(deadline).await {
            return Err(Status::TimedOut);
        }
    }
}

/// The change of child `pid` that `wait` found: its termination, which is
/// collected, or else its stop when `stops`, or else its continuation,
/// which are reported once.
fn report(shared: &Shared, pid: u32, stops: bool) -> Result<LauncherWaitResponse, Status> {
    let status = |kind, exit_code, term_signal, stop_signal| LauncherWaitResponse {
        pid,
        status: ExitStatus {
            kind,
            exit_code,
            term_signal,
            stop_signal,
        },
    };
    let mut records = shared.records.borrow_mut();
    let record = records.get_mut(pid).ok_or(Status::BadState)?;
    if record.exit.is_some() {
        let Exit { code, signal } = records.collect(pid).ok_or(Status::BadState)?;
        return Ok(status(StatusKind::Exited, code, signal, 0));
    }
    if let Some(signo) = record.stopped.take().filter(|_| stops) {
        return Ok(status(StatusKind::Stopped, 0, 0, signo));
    }
    record.continued = false;
    Ok(status(StatusKind::Continued, 0, 0, 0))
}

/// Whether the broker says that process `caller` holds `permission`.
async fn holds(shared: &Shared, caller: u32, permission: &str) -> bool {
    let client = {
        let records = shared.records.borrow();
        let Some(from) = records.get(caller) else {
            return false;
        };
        ClientIdentity {
            pid: caller,
            user: from.user,
            app_id: from.app_id.clone(),
        }
    };
    let bootstrap = shared.identity_setter.get() == Some(caller);
    shared
        .permissions
        .holds(client, bootstrap, permission)
        .await
}

/// What process `caller` may learn of process `target`: all of it when
/// `target` has the caller's user ID, or when the caller holds
/// `process.list_all`; otherwise the process is reported missing.
async fn get_info(
    shared: &Shared,
    caller: u32,
    target: u32,
) -> Result<LauncherGetInfoResponse, Status> {
    let same_user = {
        let records = shared.records.borrow();
        let from = records.get(caller).ok_or(Status::BadState)?;
        let record = records.get(target).ok_or(Status::NotFound)?;
        record.user == from.user
    };
    if !same_user && !holds(shared, caller, LIST_ALL).await {
        return Err(Status::NotFound);
    }
    // The target may have been collected while the broker answered.
    let records = shared.records.borrow();
    let record = records.get(target).ok_or(Status::NotFound)?;
    Ok(LauncherGetInfoResponse {
        info: ProcessInfo {
            pid: target,
            parent: record.parent,
            process_group: record.process_group,
            session: record.session,
            login_session: record.login_session,
            user: record.user,
            app_id: record.app_id.clone(),
            path: record.path.clone(),
        },
    })
}

/// The most process IDs one `List` reply carries.
const LIST_MAX: usize = 4096;

/// The process IDs, in order, that process `caller` may see: those of
/// its user ID, ended ones that their parent has not collected included,
/// or all of them when it holds `process.list_all`. At most `LIST_MAX`
/// are given.
async fn list(shared: &Shared, caller: u32) -> LauncherListResponse {
    let all = holds(shared, caller, LIST_ALL).await;
    let records = shared.records.borrow();
    let user = records.get(caller).map(|r| r.user);
    let pids = records
        .iter()
        .filter(|(_, r)| all || Some(r.user) == user)
        .map(|(pid, _)| pid)
        .take(LIST_MAX)
        .collect();
    LauncherListResponse { pids }
}

/// Whether process `caller` may signal process `target` without asking
/// the broker: a descendant, or one with its user ID and app ID.
fn may_signal(records: &Records, caller: u32, target: u32) -> bool {
    let (Some(from), Some(to)) = (records.get(caller), records.get(target)) else {
        return false;
    };
    let same = to.user == from.user && to.app_id == from.app_id;
    same || records.descends_from(target, caller)
}

/// Sends `signo` to `record`'s process.
fn signal(record: &Record, signo: u32) -> Result<(), Status> {
    // SAFETY: the call takes no pointers.
    match unsafe { sys::process_signal(record.process.raw(), signo, 0) } {
        mikros_rt::OK => Ok(()),
        status => Err(start::status(status)),
    }
}

/// Sends `signo` to process `target` for process `caller`, which may
/// signal it as `may_signal` says, or else when the broker says that it
/// holds `process.signal_any`.
async fn kill(
    shared: &Shared,
    caller: u32,
    target: u32,
    signo: u32,
) -> Result<LauncherKillResponse, Status> {
    let client = {
        let records = shared.records.borrow();
        let from = records.get(caller).ok_or(Status::BadState)?;
        let to = records.get(target).ok_or(Status::NotFound)?;
        if may_signal(&records, caller, target) {
            return signal(to, signo).map(|()| LauncherKillResponse {});
        }
        ClientIdentity {
            pid: caller,
            user: from.user,
            app_id: from.app_id.clone(),
        }
    };
    let bootstrap = shared.identity_setter.get() == Some(caller);
    if !shared
        .permissions
        .holds(client, bootstrap, SIGNAL_ANY)
        .await
    {
        return Err(Status::AccessDenied);
    }
    // The target may have ended while the broker answered.
    let records = shared.records.borrow();
    let to = records.get(target).ok_or(Status::NotFound)?;
    signal(to, signo).map(|()| LauncherKillResponse {})
}

/// Sends `signo` for process `caller` to every running process of
/// `group` that it may signal: as `may_signal` says, or all of them when
/// the broker says that it holds `process.signal_any`, which is asked
/// only when `may_signal` leaves some out. Fails with `NOT_FOUND` when
/// the group has no running process, and with `ACCESS_DENIED` when it
/// may signal none.
async fn kill_group(
    shared: &Shared,
    caller: u32,
    group: u32,
    signo: u32,
) -> Result<LauncherKillGroupResponse, Status> {
    let left_out = {
        let records = shared.records.borrow();
        records.get(caller).ok_or(Status::BadState)?;
        let members: Vec<u32> = records
            .iter()
            .filter(|(_, r)| r.process_group == group && r.exit.is_none())
            .map(|(pid, _)| pid)
            .collect();
        if members.is_empty() {
            return Err(Status::NotFound);
        }
        members
            .iter()
            .any(|&pid| !may_signal(&records, caller, pid))
    };
    let any = left_out && holds(shared, caller, SIGNAL_ANY).await;
    // The group may have changed while the broker answered.
    let records = shared.records.borrow();
    let mut signalled = false;
    for (pid, record) in records
        .iter()
        .filter(|(_, r)| r.process_group == group && r.exit.is_none())
    {
        if any || may_signal(&records, caller, pid) {
            signal(record, signo)?;
            signalled = true;
        }
    }
    match signalled {
        true => Ok(LauncherKillGroupResponse {}),
        false => Err(Status::AccessDenied),
    }
}

/// Makes process `caller` the leader of a new session and of a new
/// process group, both with its process ID. Fails with `BAD_STATE` when
/// it leads a process group already.
fn create_session(shared: &Shared, caller: u32) -> Result<LauncherCreateSessionResponse, Status> {
    let mut records = shared.records.borrow_mut();
    let record = records.get_mut(caller).ok_or(Status::BadState)?;
    if record.process_group == caller {
        return Err(Status::BadState);
    }
    record.process_group = caller;
    record.session = caller;
    Ok(LauncherCreateSessionResponse { session: caller })
}

/// Moves process `target`, the caller or one of its children, to process
/// group `group` of its session: one that a running process of that
/// session is in, or a new one when `group` is `target`. Fails with
/// `ACCESS_DENIED` for any other process, a child in another session, or
/// another group.
fn set_process_group(
    shared: &Shared,
    caller: u32,
    target: u32,
    group: u32,
) -> Result<LauncherSetProcessGroupResponse, Status> {
    let mut records = shared.records.borrow_mut();
    let session = records.get(caller).ok_or(Status::BadState)?.session;
    let moved = records
        .get(target)
        .filter(|r| target == caller || (r.parent == caller && r.exit.is_none()))
        .filter(|r| r.session == session)
        .ok_or(Status::AccessDenied)?;
    let exists = records
        .iter()
        .any(|(_, r)| r.process_group == group && r.session == moved.session && r.exit.is_none());
    if group != target && !exists {
        return Err(Status::AccessDenied);
    }
    records
        .get_mut(target)
        .ok_or(Status::BadState)?
        .process_group = group;
    Ok(LauncherSetProcessGroupResponse {})
}

fn clone(identity: &ClientIdentity) -> ClientIdentity {
    ClientIdentity {
        pid: identity.pid,
        user: identity.user,
        app_id: identity.app_id.clone(),
    }
}
