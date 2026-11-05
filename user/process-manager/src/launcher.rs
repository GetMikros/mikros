//! The `Launcher` endpoint of each process the process manager starts.
//!
//! Each process has its own channel, so the record it is bound to is
//! the one it was created for, and requests act for that record only.
//! `Spawn`, `Exec`, `Wait` and `Kill` are served; every other method fails with
//! `NOT_SUPPORTED`, and `List`, which cannot fail, closes the channel.

use crate::processes::{NewProcess, Shared};
use crate::records::{Exit, Record};
use crate::start::{self, duplicate};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    ExitStatus, Launcher, LauncherAsyncServer, LauncherExecResponse, LauncherKillResponse,
    LauncherRequest, LauncherSpawnResponse, LauncherWaitResponse, SpawnRequest, StatusKind,
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
                    let result = wait(&shared, pid, request.selector, request.deadline).await;
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
            R::GetInfo(_, r) => r.reply(s, Err(no)),
            R::CreateSession(_, r) => r.reply(s, Err(no)),
            R::SetProcessGroup(_, r) => r.reply(s, Err(no)),
            R::KillGroup(_, r) => r.reply(s, Err(no)),
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
        Some(app_id) => named_app(shared, caller, app_id)?,
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

/// Checks that process `caller` may give a new process `app_id`, and
/// returns it.
fn named_app(shared: &Shared, caller: u32, app_id: &str) -> Result<String, Status> {
    // Until the permission broker serves requests, only the service
    // manager that init asked for holds `process.set_identity`.
    if shared.identity_setter.get() != Some(caller) {
        return Err(Status::AccessDenied);
    }
    // TODO: check bundle locations for other app IDs.
    if !app_id.starts_with(SYSTEM_PREFIX) {
        return Err(Status::AccessDenied);
    }
    // TODO: check the program manifest once it can be parsed; until
    // then an app ID that has one is refused.
    let manifest = format!("usr/lib/programs/{app_id}.yaml");
    if shared.fs.exists(&manifest) {
        return Err(Status::AccessDenied);
    }
    Ok(app_id.into())
}

/// Waits until a child of `caller` that `selector` chooses terminated,
/// or `deadline` passed, and collects its status.
async fn wait(
    shared: &Shared,
    caller: u32,
    selector: i32,
    deadline: i64,
) -> Result<LauncherWaitResponse, Status> {
    loop {
        let found = {
            let records = shared.records.borrow();
            let mut children = records.children(caller, selector).peekable();
            if children.peek().is_none() {
                return Err(Status::NotFound);
            }
            children
                .find(|(_, record)| record.exit.is_some())
                .map(|(pid, _)| pid)
        };
        if let Some(pid) = found {
            let exit = shared.records.borrow_mut().collect(pid);
            let Exit { code, signal } = exit.ok_or(Status::BadState)?;
            return Ok(LauncherWaitResponse {
                pid,
                status: ExitStatus {
                    kind: StatusKind::Exited,
                    exit_code: code,
                    term_signal: signal,
                    stop_signal: 0,
                },
            });
        }
        if !shared.termination(deadline).await {
            return Err(Status::TimedOut);
        }
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
    let from = records.get(caller).ok_or(Status::BadState)?;
    let to = records.get(target).ok_or(Status::NotFound)?;
    let same = to.user == from.user && to.app_id == from.app_id;
    // `process.signal_any` cannot be held before the permission broker.
    if !same && !records.descends_from(target, caller) {
        return Err(Status::AccessDenied);
    }
    // SAFETY: the call takes no pointers.
    match unsafe { sys::process_signal(to.process.raw(), signo, 0) } {
        mikros_rt::OK => Ok(LauncherKillResponse {}),
        status => Err(start::status(status)),
    }
}
