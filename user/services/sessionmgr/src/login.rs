//! `TextLogin`, which the text console calls: first-boot setup, and
//! logins, which start the user's session on the console's terminal and
//! end it once its leader exits.

use crate::spawn::{block, standard};
use crate::{SECOND, now};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    LauncherAsyncClient, SpawnRequest, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::session::{
    AccountsAsyncClient, TextLogin, TextLoginAsyncServer, TextLoginLoginResponse,
    TextLoginNeedsSetupResponse, TextLoginRequest, TextLoginRunSetupResponse,
};

const SETUP: &str = "/usr/bin/setup";
const SETUP_APP: &str = "system.setup";
const SHELL: &str = "/usr/bin/msh";
const SHELL_APP: &str = "system.cli";
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
/// The signals that end a session: first asked, then forced.
const TERM: u32 = 15;
const KILL: u32 = 9;
/// How long a session's processes have after `TERM`.
const GRACE: i64 = 5 * SECOND;
const PATH: &str = "PATH=/usr/bin";
const TERM_VALUE: &str = "TERM=xterm-256color";

/// Whether a user account exists.
async fn any_user(accounts: &AccountsAsyncClient) -> Result<bool, Status> {
    match accounts.list_users(0).await {
        Ok(listed) => Ok(!listed.users.is_empty()),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Starts `path` with `request`'s fields on the terminal of `token`, and
/// returns its process ID.
async fn start(
    launcher: &LauncherAsyncClient,
    path: &str,
    app_id: &str,
    user: u32,
    login_session: bool,
    environment: &[String],
    token: &OwnedHandle,
) -> Result<u32, Status> {
    let request = SpawnRequest {
        path: path.into(),
        arguments: block(&[path], environment).ok_or(Status::NoResources)?,
        handles: standard(token).ok_or(Status::NoResources)?,
        app_id: Some(app_id.into()),
        user,
        login_session,
    };
    match launcher.spawn(request).await {
        Ok(Ok(spawned)) => Ok(spawned.pid),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Waits until child `pid` has terminated, and returns its exit code,
/// or `None` when it cannot be waited for.
async fn exited(launcher: &LauncherAsyncClient, pid: u32) -> Option<i64> {
    loop {
        match launcher.wait(pid as i32, WaitOptions(0), FOREVER).await {
            Ok(Ok(w)) if w.status.kind == StatusKind::Exited => return Some(w.status.exit_code),
            Ok(Ok(_)) => continue,
            _ => return None,
        }
    }
}

/// Whether a process of login session `session` still runs.
async fn runs(launcher: &LauncherAsyncClient, session: u32) -> bool {
    let Ok(listed) = launcher.list().await else {
        return false;
    };
    for pid in listed.pids {
        if let Ok(Ok(info)) = launcher.get_info(pid).await
            && info.info.login_session == session
        {
            return true;
        }
    }
    false
}

/// Runs the setup program as user 0 on the terminal of `token`, and
/// returns once it has exited.
async fn setup(
    accounts: &AccountsAsyncClient,
    launcher: &LauncherAsyncClient,
    token: OwnedHandle,
) -> Result<TextLoginRunSetupResponse, Status> {
    if any_user(accounts).await? {
        return Err(Status::BadState);
    }
    let environment = [PATH.into(), TERM_VALUE.into()];
    let pid = start(
        launcher,
        SETUP,
        SETUP_APP,
        CALLER_USER,
        false,
        &environment,
        &token,
    )
    .await?;
    exited(launcher, pid).await;
    Ok(TextLoginRunSetupResponse {})
}

/// The environment of `name`'s session leader on the text console.
fn environment(name: &str) -> Vec<String> {
    vec![
        format!("HOME=/Users/{name}"),
        format!("USER={name}"),
        format!("LOGNAME={name}"),
        format!("SHELL={SHELL}"),
        PATH.into(),
        TERM_VALUE.into(),
    ]
}

/// Ends the login session of leader `pid` once the leader has exited:
/// `TERM` to the session, and `KILL` after `GRACE` when a process of it
/// still runs.
async fn end_session(launcher: &LauncherAsyncClient, reactor: &Reactor, pid: u32) {
    let code = exited(launcher, pid).await;
    mikros_rt::log!(
        Info,
        "sessionmgr: the leader of session {pid} exited with {code:?}"
    );
    let _ = launcher.kill_login_session(pid, TERM).await;
    let _ = reactor.sleep_until(now() + GRACE).await;
    if runs(launcher, pid).await {
        let _ = launcher.kill_login_session(pid, KILL).await;
    }
}

/// Serves `TextLogin` on `end` for as long as the console keeps it open.
/// One session runs at a time; its end is awaited by a task of its own,
/// which then sends `OnSessionEnded`.
pub async fn serve(
    end: ServerEnd<TextLogin>,
    accounts: AccountsAsyncClient,
    launcher: Rc<LauncherAsyncClient>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let server = Rc::new(TextLoginAsyncServer::new(end, reactor.clone()));
    let running = Rc::new(Cell::new(false));
    while let Ok(request) = server.next().await {
        let s = server.server();
        match request {
            TextLoginRequest::NeedsSetup(_, r) => {
                let needed = !any_user(&accounts).await.unwrap_or(true);
                let _ = r.reply(s, TextLoginNeedsSetupResponse { needed });
            }
            TextLoginRequest::RunSetup(q, r) => {
                let _ = r.reply(s, setup(&accounts, &launcher, q.terminal).await);
            }
            TextLoginRequest::Login(q, r) => {
                if running.get() {
                    let _ = r.reply(s, Err(Status::BadState));
                    continue;
                }
                let user = match accounts.authenticate(q.name.clone(), q.password).await {
                    Ok(Ok(a)) => a.user,
                    Ok(Err(_)) => {
                        let _ = r.reply(s, Err(Status::AccessDenied));
                        continue;
                    }
                    Err(_) => {
                        let _ = r.reply(s, Err(Status::PeerClosed));
                        continue;
                    }
                };
                let environment = environment(&q.name);
                let started = start(
                    &launcher,
                    SHELL,
                    SHELL_APP,
                    user,
                    true,
                    &environment,
                    &q.terminal,
                )
                .await;
                let pid = match started {
                    Ok(pid) => pid,
                    Err(status) => {
                        mikros_rt::log!(Error, "sessionmgr: starting {SHELL} failed: {status:?}");
                        let _ = r.reply(s, Err(status));
                        continue;
                    }
                };
                mikros_rt::log!(Info, "sessionmgr: {} logged in, session {pid}", q.name);
                running.set(true);
                let _ = r.reply(s, Ok(TextLoginLoginResponse {}));
                let (launcher, reactor) = (launcher.clone(), reactor.clone());
                let (server, running) = (server.clone(), running.clone());
                spawner.spawn(async move {
                    end_session(&launcher, &reactor, pid).await;
                    mikros_rt::log!(Info, "sessionmgr: ended session {pid}");
                    running.set(false);
                    let _ = server.server().send_on_session_ended();
                });
            }
        }
    }
}
