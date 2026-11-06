//! The `Launcher` endpoint of each process the process manager starts.
//!
//! Each process has its own channel, so the record it is bound to is
//! the one it was created for, and requests act for that record only.
//! `Spawn`, `Exec`, `Wait`, `Kill`, `GetInfo`, `CreateSession`,
//! `SetProcessGroup` and `KillGroup` are served; every other method fails
//! with `NOT_SUPPORTED`, and `List`, which cannot fail, closes the
//! channel.

use crate::processes::{NewProcess, Shared};
use crate::records::{Exit, Record, Records};
use crate::start::{self, duplicate};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    ExitStatus, Launcher, LauncherAsyncServer, LauncherCreateSessionResponse, LauncherExecResponse,
    LauncherGetInfoResponse, LauncherKillGroupResponse, LauncherKillResponse, LauncherRequest,
    LauncherSetProcessGroupResponse, LauncherSpawnResponse, LauncherWaitResponse, ProcessInfo,
    SpawnRequest, StatusKind, WaitOptions,
};
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
            R::Kill(request, r) => r.reply(s, kill(&shared, pid, request.pid, request.signo)),
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
            R::GetInfo(request, r) => r.reply(s, get_info(&shared, pid, request.pid)),
            R::CreateSession(_, r) => r.reply(s, create_session(&shared, pid)),
            R::SetProcessGroup(request, r) => r.reply(
                s,
                set_process_group(&shared, pid, request.pid, request.process_group),
            ),
            R::KillGroup(request, r) => r.reply(
                s,
                kill_group(&shared, pid, request.process_group, request.signo),
            ),
            R::Fork(_, r) => r.reply(s, Err(no)),
            R::KillLoginSession(_, r) => r.reply(s, Err(no)),
            // TODO: list the processes the caller may see.
            R::List(..) => return,
        };
    }
}

/// The process that `request` of process `caller` starts with the
/// parent `parent`, and whether it keeps the caller's user and app ID.
fn new_process(
    shared: &Shared,
    caller: u32,
    parent: impl FnOnce(&Record) -> u32,
    request: SpawnRequest,
) -> Result<(NewProcess, bool), Status> {
    // TODO: resolve the path in the caller's namespace (MIKSPEC-0050.R3)
    // rather than the boot file system.
    let program = request.path.strip_prefix('/').ok_or(Status::InvalidArgs)?;
    // Without the permission broker no caller may name a user or start
    // a login session.
    if request.user != CALLER_USER || request.login_session {
        return Err(Status::AccessDenied);
    }
    let records = shared.records.borrow();
    let from = records.get(caller).ok_or(Status::BadState)?;
    let app_id = match &request.app_id {
        None => from.app_id.clone(),
        Some(app_id) => named_app(shared, caller, app_id, program)?,
    };
    let same = app_id == from.app_id;
    let new = NewProcess {
        program: program.into(),
        path: request.path.clone(),
        parent: parent(from),
        process_group: from.process_group,
        session: from.session,
        app_id,
        user: from.user,
        login_session: from.login_session,
        arguments: duplicate(request.arguments.raw(), SAME_RIGHTS)?,
        handles: request.handles,
    };
    Ok((new, same))
}

/// Starts the program of `request` as a child of process `caller`.
async fn spawn(
    shared: &Rc<Shared>,
    caller: u32,
    request: SpawnRequest,
) -> Result<LauncherSpawnResponse, Status> {
    let (new_app, same) = new_process(shared, caller, |_| caller, request)?;
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
    let (new, _) = new_process(shared, caller, |from| from.parent, request)?;
    let control = shared.control.borrow().clone().ok_or(Status::BadState)?;
    shared.replace(caller, new, &control).await?;
    Ok(LauncherExecResponse {})
}

/// Checks that process `caller` may give the new process of `program`, a
/// path relative to the boot file system's root, `app_id`, and returns
/// it. An app ID outside `system.` needs the program to lie in the app's
/// bundle under `/Apps`. A `system.` app ID with a program manifest needs
/// a manifest that is valid, the program to be its `binary`, and the
/// caller's app ID in its `spawned_by`; one whose manifest cannot be used
/// is refused.
fn named_app(shared: &Shared, caller: u32, app_id: &str, program: &str) -> Result<String, Status> {
    // Until the permission broker serves requests, only the service
    // manager that init asked for holds `process.set_identity`, and no
    // process holds `files.full_disk`, which `system.cli` needs.
    if shared.identity_setter.get() != Some(caller) {
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
    let Some(bytes) = shared.fs.file(&format!("usr/lib/programs/{file_name}")) else {
        return Ok(app_id.into());
    };
    let manifest =
        mikros_manifest::parse_program(&file_name, bytes).map_err(|_| Status::AccessDenied)?;
    let program_manifest = manifest.program;
    let service = shared.fs.exists(&format!("usr/lib/services/{name}.yaml"));
    let binary = program_manifest.binary.trim_start_matches('/');
    let records = shared.records.borrow();
    let from = records.get(caller).ok_or(Status::BadState)?;
    let allowed = !service
        && shared.fs.same_file(program, binary)
        && program_manifest.spawned_by.contains(&from.app_id);
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

/// What process `caller` may know of process `target`: the whole record,
/// when `target` has the caller's user ID. Other processes are reported
/// missing, as `process.list_all` cannot be held before the permission
/// broker serves requests.
fn get_info(shared: &Shared, caller: u32, target: u32) -> Result<LauncherGetInfoResponse, Status> {
    let records = shared.records.borrow();
    let from = records.get(caller).ok_or(Status::BadState)?;
    let record = records
        .get(target)
        .filter(|r| r.user == from.user)
        .ok_or(Status::NotFound)?;
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

/// Whether process `caller` may signal process `target`: a descendant,
/// or one with its user ID and app ID. `process.signal_any` cannot be
/// held before the permission broker serves requests.
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

/// Sends `signo` to process `target` for process `caller`.
fn kill(
    shared: &Shared,
    caller: u32,
    target: u32,
    signo: u32,
) -> Result<LauncherKillResponse, Status> {
    let records = shared.records.borrow();
    records.get(caller).ok_or(Status::BadState)?;
    let to = records.get(target).ok_or(Status::NotFound)?;
    if !may_signal(&records, caller, target) {
        return Err(Status::AccessDenied);
    }
    signal(to, signo).map(|()| LauncherKillResponse {})
}

/// Sends `signo` for process `caller` to every running process of
/// `group` that it may signal. Fails with `NOT_FOUND` when the group has
/// no running process, and with `ACCESS_DENIED` when it may signal none.
fn kill_group(
    shared: &Shared,
    caller: u32,
    group: u32,
    signo: u32,
) -> Result<LauncherKillGroupResponse, Status> {
    let records = shared.records.borrow();
    records.get(caller).ok_or(Status::BadState)?;
    let mut members = records
        .iter()
        .filter(|(_, r)| r.process_group == group && r.exit.is_none())
        .peekable();
    if members.peek().is_none() {
        return Err(Status::NotFound);
    }
    let mut signalled = false;
    for (pid, record) in members {
        if may_signal(&records, caller, pid) {
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
