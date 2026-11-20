//! The graphical login. A desktop counts as installed when the
//! compositor's program manifest exists and `/system/config/session.yaml`
//! does not set `desktop` to `false`. The compositor then runs in greeter
//! mode, as user 0, with a `Login` channel, which the session manager
//! serves, and a `Seat` channel, through which it gets the display and the
//! input devices. When it has exited three times within 60 seconds, the
//! text console's login takes over until the next boot.

use crate::spawn::block;
use crate::{SECOND, now};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::{
    Control, ControlAsyncClient, DirectoryAsyncClient, ShutdownKind,
};
use mikros_rt::interfaces::mikros::session::{
    AccountsAsyncClient, Login, LoginAsyncServer, LoginListUsersResponse, LoginLoginResponse,
    LoginRequest, LoginShutdownResponse, LoginUser, Seat, SeatAsyncClient,
};

const COMPOSITOR: &str = "/system/bin/compositor";
const COMPOSITOR_APP: &str = "system.compositor";
/// The compositor's program manifest, below `/system`, and the session
/// settings, below `/system/config`.
const MANIFEST: &str = "programs/system.compositor.yaml";
const SETTINGS: &str = "session.yaml";
const CONTROL: &str = "mikros.service.Control";
/// The `USER` startup handle IDs of the compositor's channels.
const LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
/// How many exits within how long make the console take over.
const EXITS: usize = 3;
const EXIT_WINDOW: i64 = 60 * SECOND;
const FOREVER: i64 = i64::MAX;

/// Whether a desktop is installed, from the files under `system` and
/// `config`, the directories of `/system` and `/system/config`.
pub async fn installed(
    system: &NodeAsyncClient,
    config: &NodeAsyncClient,
    reactor: &Reactor,
) -> bool {
    match files::read(system, reactor, MANIFEST, mikros_yaml::SIZE_MAX).await {
        Ok(_) => {}
        Err(Failure::Fs(Error::NotFound)) => return false,
        Err(e) => {
            mikros_rt::log!(Error, "sessionmgr: reading /system/{MANIFEST}: {e:?}");
            return false;
        }
    }
    match files::read(config, reactor, SETTINGS, mikros_yaml::SIZE_MAX).await {
        Ok(bytes) => desktop_setting(&bytes) != Some(false),
        Err(Failure::Fs(Error::NotFound)) => true,
        Err(e) => {
            mikros_rt::log!(
                Error,
                "sessionmgr: reading /system/config/{SETTINGS}: {e:?}"
            );
            true
        }
    }
}

/// The `desktop` key of the session settings in `bytes`, or None when the
/// file does not set it. An invalid file, which is logged, sets nothing.
fn desktop_setting(bytes: &[u8]) -> Option<bool> {
    let invalid = |line: usize, column: usize| {
        mikros_rt::log!(
            Error,
            "sessionmgr: /system/config/{SETTINGS} is invalid at line {line}, column {column}"
        );
    };
    let root = match mikros_yaml::parse(bytes) {
        Ok(root) => root,
        Err(e) => {
            invalid(e.line, e.column);
            return None;
        }
    };
    let map = match root.map() {
        Ok(map) => map,
        Err(e) => {
            invalid(e.line, e.column);
            return None;
        }
    };
    for entry in map.unknown(&["desktop"]) {
        let key = &entry.key;
        mikros_rt::log!(
            Info,
            "sessionmgr: /system/config/{SETTINGS}: ignored the key {key}"
        );
    }
    match map.get("desktop")?.boolean() {
        Ok(desktop) => Some(desktop),
        Err(e) => {
            invalid(e.line, e.column);
            None
        }
    }
}

/// Runs the compositor in greeter mode, serving its `Login` channel and
/// giving it the seat, and starts it again whenever it exits. Returns once
/// it has exited three times within 60 seconds.
pub async fn run(
    directory: Rc<DirectoryAsyncClient>,
    launcher: Rc<LauncherAsyncClient>,
    accounts: Rc<AccountsAsyncClient>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let mut exits = VecDeque::new();
    loop {
        let (Ok((seat, seat_server)), Ok((login_client, login_server))) =
            (endpoints::<Seat>(), endpoints::<Login>())
        else {
            return;
        };
        let Some(arguments) = block(&[COMPOSITOR], &[]) else {
            return;
        };
        let request = SpawnRequest {
            path: COMPOSITOR.into(),
            arguments,
            handles: vec![
                StartupHandle {
                    role: HandleRole::User,
                    id: LOGIN_ID,
                    handle: login_client.into_handle(),
                },
                StartupHandle {
                    role: HandleRole::User,
                    id: SEAT_ID,
                    handle: seat_server.into_handle(),
                },
            ],
            app_id: Some(COMPOSITOR_APP.into()),
            user: 0,
            login_session: false,
            new_session: false,
            working_directory: None,
            umask: u32::MAX,
        };
        let pid = match launcher.spawn(request).await {
            Ok(Ok(spawned)) => spawned.pid,
            other => {
                mikros_rt::log!(
                    Error,
                    "sessionmgr: starting the compositor failed: {other:?}"
                );
                return;
            }
        };
        mikros_rt::log!(
            Info,
            "sessionmgr: started the compositor in greeter mode as pid {pid}"
        );
        let seat = SeatAsyncClient::new(seat, reactor.clone());
        let (d, r) = (directory.clone(), reactor.clone());
        spawner.spawn(async move { crate::seat::serve(&d, &r, seat, "compositor").await });
        let login =
            LoginAsyncServer::new(ServerEnd::new(login_server.into_handle()), reactor.clone());
        let (d, a, r) = (directory.clone(), accounts.clone(), reactor.clone());
        spawner.spawn(async move { serve(login, &d, &a, &r).await });
        exited(&launcher, pid).await;
        let at = now();
        exits.push_back(at);
        while exits.front().is_some_and(|&t| at - t > EXIT_WINDOW) {
            exits.pop_front();
        }
        mikros_rt::log!(Error, "sessionmgr: the compositor in greeter mode exited");
        if exits.len() >= EXITS {
            return;
        }
    }
}

/// Waits until child `pid` has terminated, or cannot be waited for.
async fn exited(launcher: &LauncherAsyncClient, pid: u32) {
    loop {
        match launcher.wait(pid as i32, WaitOptions(0), FOREVER).await {
            Ok(Ok(w)) if w.status.kind != StatusKind::Exited => continue,
            _ => return,
        }
    }
}

/// Serves `Login` until the compositor closes it.
async fn serve(
    login: LoginAsyncServer,
    directory: &DirectoryAsyncClient,
    accounts: &AccountsAsyncClient,
    reactor: &Reactor,
) {
    while let Ok(request) = login.next().await {
        let s = login.server();
        let _ = match request {
            LoginRequest::ListUsers(q, r) => {
                let users = accounts
                    .list_users(q.after)
                    .await
                    .map(|l| l.users)
                    .unwrap_or_default();
                let last = users.last().map_or(0, |u| u.id);
                let users = users
                    .into_iter()
                    .map(|u| LoginUser {
                        name: u.name,
                        full_name: u.full_name,
                    })
                    .collect();
                r.reply(s, LoginListUsersResponse { users, last })
            }
            LoginRequest::Login(q, r) => {
                let result = match accounts.authenticate(q.name, q.password).await {
                    // Sessions of the desktop come with the compositor of
                    // a user session.
                    Ok(Ok(_)) => Err(Status::NotSupported),
                    Ok(Err(_)) => Err(Status::AccessDenied),
                    Err(_) => Err(Status::PeerClosed),
                };
                r.reply(s, result.map(|()| LoginLoginResponse {}))
            }
            LoginRequest::Shutdown(q, r) => {
                let result = shutdown(directory, reactor, q.kind).await;
                r.reply(s, result.map(|()| LoginShutdownResponse {}))
            }
        };
    }
}

/// Asks the service manager to power off or restart the machine.
async fn shutdown(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    kind: ShutdownKind,
) -> Result<(), Status> {
    let (control, server) = endpoints::<Control>().map_err(|_| Status::NoResources)?;
    match directory
        .connect(CONTROL.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => {}
        _ => return Err(Status::AccessDenied),
    }
    let control = ControlAsyncClient::new(control, reactor.clone());
    match control.shutdown(kind).await {
        Ok(result) => result.map(|_| ()),
        Err(_) => Err(Status::PeerClosed),
    }
}
