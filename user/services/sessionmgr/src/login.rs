//! `TextLogin`, which the text console calls for each of its consoles,
//! numbered 1 to 6: first-boot setup, and logins, which start the user's
//! session on the console's terminal and end it once its leader exits.
//! Each console has at most one session, and the sessions of different
//! consoles run at the same time. A user whom the greeter authenticated
//! is handed to the lowest console without a session: `OnHandover` tells
//! the console a new random number, with which it calls `LoginHandover`
//! within 30 seconds (MIKSPEC-0057.R40).

use crate::authorize::Requests;
use crate::spawn::{block, standard};
use crate::wait::{Either, Slot, select};
use crate::{SECOND, now};
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    LauncherAsyncClient, SpawnRequest, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::session::{
    AccountsAsyncClient, TextLogin, TextLoginAnswerAskResponse, TextLoginAnswerResponse,
    TextLoginAsyncServer, TextLoginCancelResponse, TextLoginLoginHandoverResponse,
    TextLoginLoginResponse, TextLoginNeedsSetupResponse, TextLoginRequest,
    TextLoginRunSetupResponse,
};

const SETUP: &str = "/system/bin/setup";
const SETUP_APP: &str = "system.setup";
const SHELL: &str = "/system/bin/msh";
const SHELL_APP: &str = "system.cli";
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
/// The signals that end a session: first asked, then forced.
const TERM: u32 = 15;
const KILL: u32 = 9;
/// How long a session's processes have after `TERM`.
const GRACE: i64 = 5 * SECOND;
/// The commands of the system, then those of packages (MIKSPEC-0101.R44).
const PATH: &str = "PATH=/system/bin:/system/commands";
const TERM_VALUE: &str = "TERM=xterm-256color";
/// The numbers of the consoles, and the one that setup runs on.
const CONSOLES: core::ops::RangeInclusive<u8> = 1..=6;
const SETUP_CONSOLE: u8 = 1;
/// How long a handover's number stays valid.
const HANDOVER_FOR: i64 = 30 * SECOND;

/// A session handed to a console, until the console starts it.
pub struct Handover {
    number: u64,
    user: u32,
    name: String,
    at: i64,
    /// Whether the console started the session.
    started: Rc<Slot<Result<(), Status>>>,
}

/// Hands the session of `user`, named `name`, whom the greeter
/// authenticated, to the lowest console without a session, and returns
/// that console once the session runs there. Fails with `BAD_STATE`
/// without such a console or a console that serves `TextLogin`, and with
/// `TIMED_OUT` when the console does not start the session in time.
pub async fn hand_over(requests: &Requests, user: u32, name: &str) -> Result<u8, Status> {
    let server = requests
        .text_login
        .borrow()
        .clone()
        .ok_or(Status::BadState)?;
    let console = CONSOLES
        .clone()
        .find(|c| {
            !requests.sessions.borrow().contains_key(c)
                && !requests.handovers.borrow().contains_key(c)
        })
        .ok_or(Status::BadState)?;
    let mut number = [0u8; 8];
    mikros_rt::objects::random(&mut number).map_err(|_| Status::Internal)?;
    let number = u64::from_le_bytes(number);
    let started = Rc::new(Slot::default());
    let at = now();
    requests.handovers.borrow_mut().insert(
        console,
        Handover {
            number,
            user,
            name: name.into(),
            at,
            started: started.clone(),
        },
    );
    if server
        .server()
        .send_on_handover(console, number, name.into())
        .is_err()
    {
        requests.handovers.borrow_mut().remove(&console);
        return Err(Status::PeerClosed);
    }
    let late = requests.reactor.sleep_until(at + HANDOVER_FOR);
    match select(started.take(), late).await {
        Either::Left(started) => started.map(|()| console),
        Either::Right(_) => {
            let mut handovers = requests.handovers.borrow_mut();
            if handovers.get(&console).is_some_and(|h| h.number == number) {
                handovers.remove(&console);
            }
            Err(Status::TimedOut)
        }
    }
}

/// What starting a session needs, and the consoles with one.
struct Sessions {
    launcher: Rc<LauncherAsyncClient>,
    requests: Rc<Requests>,
    server: Rc<TextLoginAsyncServer>,
    running: Rc<RefCell<BTreeSet<u8>>>,
    reactor: Reactor,
    spawner: Spawner,
}

impl Sessions {
    /// Starts the session of `user`, named `name`, on `console` with the
    /// terminal of `token`, and a task that ends it once its leader exits
    /// and then sends `OnSessionEnded`.
    async fn start(
        &self,
        console: u8,
        user: u32,
        name: String,
        token: &OwnedHandle,
    ) -> Result<(), Status> {
        let environment = environment(&name);
        let home = Some(format!("/users/{name}"));
        let started = start(
            &self.launcher,
            SHELL,
            SHELL_APP,
            user,
            home,
            &environment,
            token,
        );
        let pid = match started.await {
            Ok(pid) => pid,
            Err(status) => {
                mikros_rt::log!(Error, "sessionmgr: starting {SHELL} failed: {status:?}");
                return Err(status);
            }
        };
        mikros_rt::log!(
            Info,
            "sessionmgr: {name} logged in on console {console}, session {pid}"
        );
        self.running.borrow_mut().insert(console);
        // The session's login session has the shell's process ID.
        self.requests.sessions.borrow_mut().insert(console, pid);
        let (launcher, reactor) = (self.launcher.clone(), self.reactor.clone());
        let (server, running) = (self.server.clone(), self.running.clone());
        let requests = self.requests.clone();
        self.spawner.spawn(async move {
            end_session(&launcher, &reactor, pid).await;
            mikros_rt::log!(Info, "sessionmgr: ended session {pid}");
            requests.sessions.borrow_mut().remove(&console);
            running.borrow_mut().remove(&console);
            let _ = server.server().send_on_session_ended(console);
        });
        Ok(())
    }

    /// Starts the session handed to `console` with `number`, without a
    /// password, on the terminal of `token`.
    async fn hand_over(&self, console: u8, number: u64, token: &OwnedHandle) -> Result<(), Status> {
        let handover = {
            let mut handovers = self.requests.handovers.borrow_mut();
            match handovers.get(&console) {
                Some(h) if h.number == number => handovers.remove(&console),
                _ => None,
            }
        };
        let handover = handover.ok_or(Status::NotFound)?;
        if now() - handover.at > HANDOVER_FOR {
            return Err(Status::NotFound);
        }
        let started = match self.running.borrow().contains(&console) {
            true => Err(Status::BadState),
            false => Ok(()),
        };
        let started = match started {
            Ok(()) => {
                self.start(console, handover.user, handover.name, token)
                    .await
            }
            Err(status) => Err(status),
        };
        handover.started.give(started);
        started
    }
}

/// Whether a user account exists.
async fn any_user(accounts: &AccountsAsyncClient) -> Result<bool, Status> {
    match accounts.list_users(0).await {
        Ok(listed) => Ok(!listed.users.is_empty()),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Starts `path` with `request`'s fields on the terminal of `token`, and
/// returns its process ID. A user's session leader, which is given the
/// user's `home`, starts in that folder, in a login session of its own,
/// and as the leader of a new session, whose controlling terminal the C
/// library makes that terminal.
async fn start(
    launcher: &LauncherAsyncClient,
    path: &str,
    app_id: &str,
    user: u32,
    home: Option<String>,
    environment: &[String],
    token: &OwnedHandle,
) -> Result<u32, Status> {
    let leader = home.is_some();
    let request = SpawnRequest {
        path: path.into(),
        arguments: block(&[path], environment).ok_or(Status::NoResources)?,
        handles: standard(token).ok_or(Status::NoResources)?,
        app_id: Some(app_id.into()),
        user,
        login_session: leader,
        new_session: leader,
        working_directory: home,
        umask: u32::MAX,
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
        None,
        &environment,
        &token,
    )
    .await?;
    let code = exited(launcher, pid).await;
    mikros_rt::log!(Info, "sessionmgr: setup exited with {code:?}");
    Ok(TextLoginRunSetupResponse {})
}

/// The environment of `name`'s session leader on the text console.
fn environment(name: &str) -> Vec<String> {
    vec![
        format!("HOME=/users/{name}"),
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
/// Setup, and the end of each session, are awaited by tasks of their
/// own; the end of a session sends `OnSessionEnded` with its console.
pub async fn serve(
    end: ServerEnd<TextLogin>,
    accounts: AccountsAsyncClient,
    launcher: Rc<LauncherAsyncClient>,
    requests: Rc<Requests>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let server = Rc::new(TextLoginAsyncServer::new(end, reactor.clone()));
    *requests.text_login.borrow_mut() = Some(server.clone());
    let accounts = Rc::new(accounts);
    // The consoles on which a session runs.
    let running: Rc<RefCell<BTreeSet<u8>>> = Rc::new(RefCell::new(BTreeSet::new()));
    let sessions = Sessions {
        launcher: launcher.clone(),
        requests: requests.clone(),
        server: server.clone(),
        running: running.clone(),
        reactor: reactor.clone(),
        spawner: spawner.clone(),
    };
    // Whether the setup program runs.
    let in_setup = Rc::new(Cell::new(false));
    while let Ok(request) = server.next().await {
        let s = server.server();
        match request {
            TextLoginRequest::NeedsSetup(_, r) => {
                let needed = !any_user(&accounts).await.unwrap_or(true);
                let _ = r.reply(s, TextLoginNeedsSetupResponse { needed });
            }
            TextLoginRequest::RunSetup(q, r) => {
                // Setup runs on console 1 only, once at a time.
                if q.console != SETUP_CONSOLE || in_setup.get() {
                    let _ = r.reply(s, Err(Status::BadState));
                    continue;
                }
                in_setup.set(true);
                let (accounts, launcher, server) =
                    (accounts.clone(), launcher.clone(), server.clone());
                let in_setup = in_setup.clone();
                spawner.spawn(async move {
                    let done = setup(&accounts, &launcher, q.terminal).await;
                    in_setup.set(false);
                    let _ = r.reply(server.server(), done);
                });
            }
            TextLoginRequest::Login(q, r) => {
                if !CONSOLES.contains(&q.console) {
                    let _ = r.reply(s, Err(Status::InvalidArgs));
                    continue;
                }
                if running.borrow().contains(&q.console) {
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
                let started = sessions.start(q.console, user, q.name, &q.terminal).await;
                let _ = r.reply(s, started.map(|()| TextLoginLoginResponse {}));
            }
            TextLoginRequest::LoginHandover(q, r) => {
                let started = sessions.hand_over(q.console, q.handover, &q.terminal).await;
                let _ = r.reply(s, started.map(|()| TextLoginLoginHandoverResponse {}));
            }
            // The password is checked in a task of its own, so that the
            // console's other requests, Cancel among them, go on.
            TextLoginRequest::Answer(q, r) => {
                let (requests, server) = (requests.clone(), server.clone());
                spawner.spawn(async move {
                    let answered = requests.answer(q.request, q.name, q.password).await;
                    let reply = answered.map(|()| TextLoginAnswerResponse {});
                    let _ = r.reply(server.server(), reply);
                });
            }
            TextLoginRequest::AnswerAsk(q, r) => {
                let answered = requests.answer_ask(q.request, q.allowed);
                let _ = r.reply(s, answered.map(|()| TextLoginAnswerAskResponse {}));
            }
            TextLoginRequest::Cancel(q, r) => {
                let cancelled = requests.cancel(q.request);
                let _ = r.reply(s, cancelled.map(|()| TextLoginCancelResponse {}));
            }
        }
    }
}
