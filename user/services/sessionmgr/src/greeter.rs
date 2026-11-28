//! The graphical login. A desktop counts as installed when the
//! compositor's program manifest exists and `/system/config/session.yaml`
//! does not set `desktop` to `false`. The compositor then runs in greeter
//! mode, as the greeter account, with a `Login` channel, which the session
//! manager serves, and a `Seat` channel, through which it gets the display
//! and the input devices. When it has exited three times within 60
//! seconds, the text console's login takes over until the next boot.

use crate::authorize::Requests;
use crate::seat::Seats;
use crate::spawn::block;
use crate::{SECOND, now};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::desktop::{Compositor, CompositorAsyncClient};
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::{
    Control, ControlAsyncClient, DirectoryAsyncClient, ShutdownKind,
};
use mikros_rt::interfaces::mikros::session::{
    AccountsAsyncClient, Login, LoginAsyncServer, LoginListUsersResponse, LoginLoginResponse,
    LoginMode, LoginRequest, LoginShutdownResponse, LoginUser, Seat, SeatAsyncClient,
};

const COMPOSITOR: &str = "/system/bin/compositor";
const COMPOSITOR_APP: &str = "system.compositor";
/// The compositor's program manifest, below `/system`, and the session
/// settings, below `/system/config`.
const MANIFEST: &str = "programs/system.compositor.yaml";
const SETTINGS: &str = "session.yaml";
const CONTROL: &str = "mikros.service.Control";
/// The `USER` startup handle IDs of the compositor's channels.
const CONNECTIONS_ID: u32 = 1;
const LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
/// How many exits within how long make the console take over.
const EXITS: usize = 3;
const EXIT_WINDOW: i64 = 60 * SECOND;
const FOREVER: i64 = i64::MAX;
/// The user ID of the greeter account, which the compositor in greeter
/// mode runs as.
pub const GREETER_USER: u32 = 1;

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
/// it has exited three times within 60 seconds. While a desktop started
/// from a text login runs, it does not run, and its ending then does not
/// count as an exit.
pub async fn run(
    directory: Rc<DirectoryAsyncClient>,
    launcher: Rc<LauncherAsyncClient>,
    accounts: Rc<AccountsAsyncClient>,
    requests: Rc<Requests>,
    seats: Rc<Seats>,
    reactor: Reactor,
    spawner: Spawner,
) {
    let mut exits = VecDeque::new();
    let desktops = &requests.desktops;
    desktops.wanted.set(true);
    loop {
        desktops
            .changed
            .until(|| desktops.wanted.get() && desktops.linked.borrow().is_none())
            .await;
        let (Ok((seat, seat_server)), Ok((login_client, login_server))) =
            (endpoints::<Seat>(), endpoints::<Login>())
        else {
            return;
        };
        let Ok((connections, connections_server)) = endpoints::<Compositor>() else {
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
                    id: CONNECTIONS_ID,
                    handle: connections_server.into_handle(),
                },
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
            user: GREETER_USER,
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
        desktops.greeter.set(Some(pid));
        *desktops.greeter_connections.borrow_mut() = Some(Rc::new(CompositorAsyncClient::new(
            connections,
            reactor.clone(),
        )));
        let seat = Rc::new(SeatAsyncClient::new(seat, reactor.clone()));
        spawner.spawn(seats.clone().switches(requests.clone(), seat.clone()));
        seats.set_greeter(seat.clone()).await;
        let login =
            LoginAsyncServer::new(ServerEnd::new(login_server.into_handle()), reactor.clone());
        let (d, a, r) = (directory.clone(), accounts.clone(), reactor.clone());
        let (q, s, sp) = (requests.clone(), seats.clone(), spawner.clone());
        spawner.spawn(async move { serve(login, &d, &a, &q, &s, &r, &sp).await });
        exited(&launcher, pid).await;
        *desktops.greeter_connections.borrow_mut() = None;
        desktops.greeter.set(None);
        desktops.changed.wake_all();
        seats.release_greeter(&seat).await;
        if desktops.ended.replace(false) {
            mikros_rt::log!(Info, "sessionmgr: ended the compositor in greeter mode");
            continue;
        }
        let at = now();
        exits.push_back(at);
        while exits.front().is_some_and(|&t| at - t > EXIT_WINDOW) {
            exits.pop_front();
        }
        mikros_rt::log!(Error, "sessionmgr: the compositor in greeter mode exited");
        if exits.len() >= EXITS {
            desktops.fallback.set(true);
            seats.switch(seats.console_screen(1)).await;
            return;
        }
    }
}

/// Which login session ended.
pub enum Ended {
    /// The session of a desktop, which has screen 1.
    Desktop,
    /// The session on text console `console`, whose login session has
    /// the process ID `pid`.
    Console(u8, u32),
}

/// Shows the login again after a login session ended (MIKSPEC-0057.R44):
/// when a desktop is installed and the text console's login has not taken
/// over, wants the compositor in greeter mode unless a compositor of a
/// user's session runs, and switches to screen 1 when the screen of the
/// session that ended is the one shown.
pub async fn returned(requests: &Rc<Requests>, ended: Ended) {
    let Some(seats) = requests.seats.borrow().clone() else {
        return;
    };
    let desktops = &requests.desktops;
    // The desktop that was started from the session ends with it, and is
    // no compositor of a user's session by now.
    if let Ended::Console(_, pid) = ended {
        desktops
            .changed
            .until(|| {
                !desktops
                    .linked
                    .borrow()
                    .is_some_and(|l| l.text_session == Some(pid))
            })
            .await;
    }
    if !seats.desktop_installed() || desktops.fallback.get() || desktops.linked.borrow().is_some() {
        return;
    }
    desktops.wanted.set(true);
    desktops.changed.wake_all();
    let screen = match ended {
        Ended::Desktop => 1,
        Ended::Console(console, _) => seats.console_screen(console),
    };
    if seats.shown() == screen {
        seats.switch(1).await;
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
    directory: &Rc<DirectoryAsyncClient>,
    accounts: &AccountsAsyncClient,
    requests: &Rc<Requests>,
    seats: &Seats,
    reactor: &Reactor,
    spawner: &Spawner,
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
                let result = match accounts.authenticate(q.name.clone(), q.password).await {
                    Ok(Ok(a)) => {
                        log_in(
                            accounts, requests, directory, seats, spawner, a.user, &q.name,
                        )
                        .await
                    }
                    Ok(Err(_)) => Err(Status::AccessDenied),
                    Err(_) => Err(Status::PeerClosed),
                };
                let ends_greeter = matches!(result, Ok(true));
                let replied = r.reply(s, result.map(|_| LoginLoginResponse {}));
                // The greeter's compositor ends after the reply to `Login`
                // (MIKSPEC-0057.R42).
                if ends_greeter {
                    crate::desktop::end_greeter(requests).await;
                }
                replied
            }
            LoginRequest::Shutdown(q, r) => {
                let result = shutdown(directory, reactor, q.kind).await;
                r.reply(s, result.map(|()| LoginShutdownResponse {}))
            }
        };
    }
}

/// Starts the session of `user`, named `name`, whom `Login`
/// authenticated: a user whose login mode is `CONSOLE` on the lowest text
/// console without a session, in the order of MIKSPEC-0057.R46: its screen
/// is switched to, the greeter's compositor is ended and the session is
/// started, and when that fails the login screen comes back and `Login`
/// fails with `IO`; and a user whose login mode is `DESKTOP` in a desktop
/// of its own, whose compositor has the seat when this returns
/// (MIKSPEC-0057.R42). It says whether the greeter's compositor has to be
/// ended once the reply has gone: after a desktop.
async fn log_in(
    accounts: &AccountsAsyncClient,
    requests: &Rc<Requests>,
    directory: &Rc<DirectoryAsyncClient>,
    seats: &Seats,
    spawner: &Spawner,
    user: u32,
    name: &str,
) -> Result<bool, Status> {
    match accounts.get_login_mode(user).await {
        Ok(Ok(r)) if r.mode == LoginMode::Console => {
            let reserved = crate::login::reserve_console(requests, user, name)?;
            let console = reserved.console;
            seats.switch(seats.console_screen(console)).await;
            crate::desktop::end_greeter(requests).await;
            match crate::login::hand_over(requests, reserved).await {
                Ok(()) => Ok(false),
                Err(status) => {
                    mikros_rt::log!(
                        Error,
                        "sessionmgr: the session of {name} on console {console}: {status:?}"
                    );
                    let desktops = &requests.desktops;
                    desktops.wanted.set(true);
                    desktops.changed.wake_all();
                    seats.switch(1).await;
                    Err(Status::Io)
                }
            }
        }
        Ok(Ok(_)) => {
            crate::desktop::start_from_greeter(requests, directory, spawner, user, name).await?;
            Ok(true)
        }
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Asks the service manager to power off or restart the machine.
pub async fn shutdown(
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
