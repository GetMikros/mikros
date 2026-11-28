//! `mikros.session.Desktop`, which the session manager serves to every
//! process: `StartDesktop` starts the desktop for the text session of the
//! caller (MIKSPEC-0057.R41). The compositor runs with the caller's user
//! ID as the leader of a login session of its own, linked to the
//! caller's: ending the caller's session ends it too. Its `Session`
//! channel ends only that login session on `Logout`. While it runs, the
//! greeter's compositor does not, and it owns the seat; once it has
//! exited, the console owns the seat on the screen of the caller's
//! console, and the greeter starts again.

use crate::authorize::{Answerer, FrontEnd, Requests};
use crate::spawn::block;
use crate::wait::Waiters;
use crate::{SECOND, now};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Spawner;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::desktop::{
    Compositor, CompositorAsyncClient, FilePicker, Opener,
};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, SpawnRequest, StartupHandle, StatusKind, WaitOptions,
};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, DirectoryAsyncClient};
use mikros_rt::interfaces::mikros::session::{
    Desktop, DesktopAsyncServer, DesktopRequest, DesktopStartDesktopResponse, Seat,
    SeatAsyncClient, Session, SessionAnswerAskResponse, SessionAnswerResponse, SessionAsyncServer,
    SessionCancelResponse, SessionLogoutResponse, SessionRequest, SessionShutdownResponse,
};

const COMPOSITOR: &str = "/system/bin/compositor";
const COMPOSITOR_APP: &str = "system.compositor";
const SHELL: &str = "/system/bin/msh";
const PATH: &str = "PATH=/system/bin";
/// The `USER` startup handle IDs of the compositor's channels.
const CONNECTIONS_ID: u32 = 1;
const SESSION_ID: u32 = 2;
const SEAT_ID: u32 = 7;
const FOREVER: i64 = i64::MAX;
/// The signals that end a login session, and how long the greeter's
/// compositor and the desktop's processes have after `TERM`.
const TERM: u32 = 15;
const KILL: u32 = 9;
const GRACE: i64 = 5 * SECOND;
/// How long a compositor that has the seat has to stay before the login is
/// answered with success.
const SETTLE: i64 = SECOND / 5;

/// The desktop that runs, linked to a text session.
#[derive(Clone, Copy, Debug)]
pub struct Linked {
    /// The compositor's process ID, which leads the desktop's login
    /// session, once it runs.
    pub leader: Option<u32>,
    /// The login session of the text session, when the desktop was
    /// started from one; none for the desktop of a login at the greeter.
    pub text_session: Option<u32>,
    /// The user of the desktop's session.
    pub user: u32,
    /// Whether the compositor has exited, while its login session is
    /// still being ended.
    pub exited: bool,
    /// Whether the text session that the desktop was started from was sent
    /// `TERM`, which ends the desktop too.
    pub text_ended: bool,
}

/// The state of desktops started from text logins.
#[derive(Default)]
pub struct Desktops {
    /// The desktop that runs or starts.
    pub linked: RefCell<Option<Linked>>,
    /// The process ID of the greeter's compositor while it runs.
    pub greeter: Cell<Option<u32>>,
    /// The `Session` server of the desktop's compositor, which its login
    /// session's questions are asked on.
    pub session: RefCell<Option<Rc<SessionAsyncServer>>>,
    /// The desktop's compositor's `Compositor` channel, on which the
    /// connections of clients are forwarded to it (MIKSPEC-0066.R40).
    pub connections: RefCell<Option<Rc<CompositorAsyncClient>>>,
    /// The same of the greeter's compositor while it runs.
    pub greeter_connections: RefCell<Option<Rc<CompositorAsyncClient>>>,
    /// Whether the compositor in greeter mode is to run when no compositor
    /// of a user's session does: at boot, after a session ended and when
    /// screen 1 is asked for, and not after it was ended for a login.
    pub wanted: Cell<bool>,
    /// Whether the session manager is ending the compositor in greeter
    /// mode itself, which is no exit of it (MIKSPEC-0075.R31).
    pub ended: Cell<bool>,
    /// Whether the compositor in greeter mode exited so often that the
    /// text console's login takes over until the next boot
    /// (MIKSPEC-0075.R32).
    pub fallback: Cell<bool>,
    /// Woken when the greeter's compositor or the desktop ended, or the
    /// greeter is wanted.
    pub changed: Waiters,
}

/// Serves `Desktop` on `end` for `client`.
pub async fn serve(
    requests: Rc<Requests>,
    directory: Rc<DirectoryAsyncClient>,
    end: ServerEnd<Desktop>,
    client: ClientIdentity,
    spawner: Spawner,
) {
    let server = DesktopAsyncServer::new(end, requests.reactor.clone());
    while let Ok(DesktopRequest::StartDesktop(_, r)) = server.next().await {
        let started = start(&requests, &directory, &client, &spawner).await;
        let _ = r.reply(
            server.server(),
            started.map(|()| DesktopStartDesktopResponse {}),
        );
    }
}

/// The name of user `user`, from the accounts service.
async fn name_of(requests: &Requests, user: u32) -> Result<String, Status> {
    let mut after = 0;
    loop {
        let users = match requests.accounts.list_users(after).await {
            Ok(listed) => listed.users,
            Err(_) => return Err(Status::PeerClosed),
        };
        if let Some(found) = users.iter().find(|u| u.id == user) {
            return Ok(found.name.clone());
        }
        after = users.last().ok_or(Status::NotFound)?.id;
    }
}

/// `StartDesktop` of `client`.
pub async fn start(
    requests: &Rc<Requests>,
    directory: &Rc<DirectoryAsyncClient>,
    client: &ClientIdentity,
    spawner: &Spawner,
) -> Result<(), Status> {
    let Some(FrontEnd::Console(console, _)) = requests.front_of(client, "StartDesktop").await
    else {
        return Err(Status::AccessDenied);
    };
    let seats = requests.seats.borrow().clone().ok_or(Status::BadState)?;
    if !seats.desktop_installed() {
        return Err(Status::NotSupported);
    }
    start_for(requests, directory, spawner, console, client.user).await
}

/// Starts the desktop of `user` for the text session on `console`.
pub async fn start_for(
    requests: &Rc<Requests>,
    directory: &Rc<DirectoryAsyncClient>,
    spawner: &Spawner,
    console: u8,
    user: u32,
) -> Result<(), Status> {
    let seats = requests.seats.borrow().clone().ok_or(Status::BadState)?;
    let desktops = &requests.desktops;
    let text_session = *requests
        .sessions
        .borrow()
        .get(&console)
        .ok_or(Status::BadState)?;
    let name = name_of(requests, user).await?;
    // No await lies between this check and the link that it makes, so that
    // two logins cannot both start a desktop.
    if desktops.linked.borrow().is_some() {
        return Err(Status::AlreadyExists);
    }
    *desktops.linked.borrow_mut() = Some(Linked {
        leader: None,
        text_session: Some(text_session),
        user,
        exited: false,
        text_ended: false,
    });
    end_greeter(requests).await;
    launch(
        requests,
        directory,
        spawner,
        &seats,
        (user, &name),
        console,
        Origin::Console,
    )
    .await
}

/// Starts the desktop of `user`, named `name`, whom the greeter logged
/// in, while the greeter's compositor keeps running: the user's compositor
/// gets the seat, and the greeter's compositor is ended afterwards by
/// `end_greeter`, once `Login` has been answered. Fails with `AlreadyExists`
/// while a desktop runs or starts, and with `Io` when the compositor does
/// not start or exits before it has the seat; the greeter then owns the seat
/// again (MIKSPEC-0057.R42).
pub async fn start_from_greeter(
    requests: &Rc<Requests>,
    directory: &Rc<DirectoryAsyncClient>,
    spawner: &Spawner,
    user: u32,
    name: &str,
) -> Result<(), Status> {
    let seats = requests.seats.borrow().clone().ok_or(Status::BadState)?;
    let desktops = &requests.desktops;
    if desktops.linked.borrow().is_some() {
        return Err(Status::AlreadyExists);
    }
    *desktops.linked.borrow_mut() = Some(Linked {
        leader: None,
        text_session: None,
        user,
        exited: false,
        text_ended: false,
    });
    launch(
        requests,
        directory,
        spawner,
        &seats,
        (user, name),
        1,
        Origin::Greeter,
    )
    .await
    .map_err(|_| Status::Io)?;
    // A compositor that exits as soon as it has the seat fails the login:
    // its end marks the link as exited, while its login session ends.
    let died = desktops
        .changed
        .until(|| desktops.linked.borrow().is_none_or(|l| l.exited));
    let settled = requests.reactor.sleep_until(now() + SETTLE);
    if let crate::wait::Either::Left(_) = crate::wait::select(died, settled).await {
        mikros_rt::log!(Error, "sessionmgr: the desktop of {name} exited at once");
        return Err(Status::Io);
    }
    Ok(())
}

/// Where the login of a desktop comes from.
#[derive(Clone, Copy)]
enum Origin {
    Console,
    Greeter,
}

/// Starts the compositor of `user`, named `name`, and gives it the seat:
/// the desktop of the link that the caller made. Once the compositor
/// has exited, the seat goes to the greeter's compositor when it runs, and
/// otherwise to the text console `console`.
async fn launch(
    requests: &Rc<Requests>,
    directory: &Rc<DirectoryAsyncClient>,
    spawner: &Spawner,
    seats: &Rc<crate::seat::Seats>,
    (user, name): (u32, &str),
    console: u8,
    origin: Origin,
) -> Result<(), Status> {
    let desktops = &requests.desktops;
    let name = String::from(name);
    let started = spawn(requests, user, &name).await;
    let (pid, seat, session, connections) = match started {
        Ok(started) => started,
        Err(status) => {
            mikros_rt::log!(
                Error,
                "sessionmgr: starting the desktop of {name}: {status:?}"
            );
            *desktops.linked.borrow_mut() = None;
            // The greeter that was ended for this desktop is wanted again,
            // since no session's compositor runs.
            desktops.wanted.set(true);
            desktops.changed.wake_all();
            return Err(status);
        }
    };
    if let Some(linked) = desktops.linked.borrow_mut().as_mut() {
        linked.leader = Some(pid);
    }
    match origin {
        Origin::Console => mikros_rt::log!(
            Info,
            "sessionmgr: started the desktop of {name} for console {console} as pid {pid}"
        ),
        Origin::Greeter => mikros_rt::log!(
            Info,
            "sessionmgr: started the desktop of {name} for the greeter's login as pid {pid}"
        ),
    }
    *desktops.connections.borrow_mut() = Some(Rc::new(CompositorAsyncClient::new(
        connections,
        requests.reactor.clone(),
    )));
    let seat = Rc::new(SeatAsyncClient::new(seat, requests.reactor.clone()));
    spawner.spawn(seats.clone().switches(requests.clone(), seat.clone()));
    let (q, d) = (requests.clone(), directory.clone());
    spawner.spawn(async move { serve_session(&q, &d, session, pid).await });
    seats.set_desktop(seat.clone()).await;
    let (q, ended_seats, ended_seat) = (requests.clone(), seats.clone(), seat);
    spawner.spawn(async move {
        wait_exit(&q, pid).await;
        mikros_rt::log!(Info, "sessionmgr: the desktop of {name} ended");
        // The seat goes back as soon as the compositor has exited, and a
        // login that waits for the compositor sees it (MIKSPEC-0057.R42);
        // its session is ended after that.
        if let Some(linked) = q.desktops.linked.borrow_mut().as_mut() {
            linked.exited = true;
        }
        q.desktops.changed.wake_all();
        // When the desktop was started from a text session that was not
        // ended, the console of that session shows again (MIKSPEC-0057.R44).
        let back = match (
            origin,
            q.desktops.linked.borrow().is_some_and(|l| l.text_ended),
        ) {
            (Origin::Console, false) => Some(ended_seats.console_screen(console)),
            _ => None,
        };
        ended_seats.release_desktop(&ended_seat, back).await;
        *q.desktops.connections.borrow_mut() = None;
        q.desktops.changed.wake_all();
        end_session(&q, pid).await;
        *q.desktops.linked.borrow_mut() = None;
        q.desktops.changed.wake_all();
        // The session of a desktop has screen 1 (MIKSPEC-0057.R44).
        crate::greeter::returned(&q, crate::greeter::Ended::Desktop).await;
    });
    Ok(())
}

/// Ends the greeter's compositor when it runs, and waits until it has.
pub async fn end_greeter(requests: &Requests) {
    let desktops = &requests.desktops;
    let Some(pid) = desktops.greeter.get() else {
        return;
    };
    desktops.ended.set(true);
    desktops.wanted.set(false);
    let _ = requests.launcher.kill(pid, TERM).await;
    let late = now() + GRACE;
    let ended = desktops.changed.until(|| desktops.greeter.get().is_none());
    let timeout = requests.reactor.sleep_until(late);
    if let crate::wait::Either::Right(_) = crate::wait::select(ended, timeout).await {
        let _ = requests.launcher.kill(pid, KILL).await;
        desktops
            .changed
            .until(|| desktops.greeter.get().is_none())
            .await;
    }
}

/// Starts the compositor of `user`, named `name`, as the leader of a new
/// login session in the user's home folder, with the environment of a
/// desktop session leader (MIKSPEC-0057.R21), and returns its process ID
/// and the session manager's ends of its `Seat` and `Session` channels.
async fn spawn(
    requests: &Requests,
    user: u32,
    name: &str,
) -> Result<
    (
        u32,
        mikros_rt::interfaces::__midl::ClientEnd<Seat>,
        ServerEnd<Session>,
        mikros_rt::interfaces::__midl::ClientEnd<Compositor>,
    ),
    Status,
> {
    let (seat, seat_server) = endpoints::<Seat>().map_err(|_| Status::NoResources)?;
    let (connections, connections_server) =
        endpoints::<Compositor>().map_err(|_| Status::NoResources)?;
    let (session_client, session) = endpoints::<Session>().map_err(|_| Status::NoResources)?;
    let environment = [
        format!("HOME=/users/{name}"),
        format!("USER={name}"),
        format!("LOGNAME={name}"),
        format!("SHELL={SHELL}"),
        PATH.into(),
    ];
    let arguments = block(&[COMPOSITOR], &environment).ok_or(Status::NoResources)?;
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
                id: SESSION_ID,
                handle: session_client.into_handle(),
            },
            StartupHandle {
                role: HandleRole::User,
                id: SEAT_ID,
                handle: seat_server.into_handle(),
            },
        ],
        app_id: Some(COMPOSITOR_APP.into()),
        user,
        login_session: true,
        new_session: false,
        working_directory: Some(format!("/users/{name}")),
        umask: u32::MAX,
    };
    match requests.launcher.spawn(request).await {
        Ok(Ok(spawned)) => Ok((spawned.pid, seat, session, connections)),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Waits until the compositor `pid` has exited.
async fn wait_exit(requests: &Requests, pid: u32) {
    loop {
        match requests
            .launcher
            .wait(pid as i32, WaitOptions(0), FOREVER)
            .await
        {
            Ok(Ok(w)) if w.status.kind != StatusKind::Exited => continue,
            _ => break,
        }
    }
}

/// Ends the login session of the compositor `pid` that has exited: `TERM`,
/// and `KILL` after `GRACE` when a process of it still runs.
async fn end_session(requests: &Requests, pid: u32) {
    let launcher = &requests.launcher;
    let _ = launcher.kill_login_session(pid, TERM).await;
    let _ = requests.reactor.sleep_until(now() + GRACE).await;
    let _ = launcher.kill_login_session(pid, KILL).await;
}

/// Ends the desktop linked to the text session `text_session`, if one
/// runs.
pub async fn end_linked(requests: &Requests, text_session: u32) {
    let linked = *requests.desktops.linked.borrow();
    if let Some(Linked {
        leader: Some(leader),
        text_session: linked,
        ..
    }) = linked
        && linked == Some(text_session)
    {
        if let Some(l) = requests.desktops.linked.borrow_mut().as_mut() {
            l.text_ended = true;
        }
        let _ = requests.launcher.kill_login_session(leader, TERM).await;
    }
}

/// Serves the compositor's `Session` channel `session` until it closes:
/// `Logout` ends the desktop's login session, led by `pid`, and
/// `Shutdown` ends it and then asks the service manager to shut down.
/// The questions of the desktop's login session are asked there, and
/// `Answer`, `AnswerAsk` and `Cancel` answer them (MIKSPEC-0075.R24).
async fn serve_session(
    requests: &Requests,
    directory: &DirectoryAsyncClient,
    session: ServerEnd<Session>,
    pid: u32,
) {
    let server = Rc::new(SessionAsyncServer::new(session, requests.reactor.clone()));
    *requests.desktops.session.borrow_mut() = Some(server.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            SessionRequest::Logout(_, r) => {
                let ended = match requests.launcher.kill_login_session(pid, TERM).await {
                    Ok(Ok(_)) => Ok(SessionLogoutResponse {}),
                    Ok(Err(status)) => Err(status),
                    Err(_) => Err(Status::PeerClosed),
                };
                r.reply(s, ended)
            }
            SessionRequest::Shutdown(q, r) => {
                let _ = requests.launcher.kill_login_session(pid, TERM).await;
                let done = crate::greeter::shutdown(directory, &requests.reactor, q.kind).await;
                r.reply(s, done.map(|()| SessionShutdownResponse {}))
            }
            SessionRequest::Answer(q, r) => {
                let answered = requests
                    .answer(Answerer::Desktop, q.request, q.name, q.password)
                    .await;
                r.reply(s, answered.map(|()| SessionAnswerResponse {}))
            }
            SessionRequest::Cancel(q, r) => {
                let cancelled = requests.cancel(Answerer::Desktop, q.request);
                r.reply(s, cancelled.map(|()| SessionCancelResponse {}))
            }
            SessionRequest::AnswerAsk(q, r) => {
                let answered = requests.answer_ask(Answerer::Desktop, q.request, q.allowed);
                r.reply(s, answered.map(|()| SessionAnswerAskResponse {}))
            }
        };
    }
    *requests.desktops.session.borrow_mut() = None;
}

/// The protocols that the session manager provides for the desktop's
/// clients, which it forwards to the compositor (MIKSPEC-0066.R40).
pub const WAYLAND: &str = "wayland";
pub const OPENER: &str = "mikros.desktop.Opener";
pub const FILE_PICKER: &str = "mikros.desktop.FilePicker";

/// Forwards the connection `server` of the client `identity` to `protocol`
/// to the compositor that runs for the client's user ID, the desktop's when
/// the client is the user of its session, the greeter's when it is the
/// greeter account, and closes it instead when there is none
/// (MIKSPEC-0066.R40). The greeter compositor takes only `wayland`.
pub async fn forward(
    requests: &Requests,
    protocol: &str,
    server: mikros_rt::interfaces::__midl::OwnedHandle,
    identity: ClientIdentity,
) {
    let linked = *requests.desktops.linked.borrow();
    let desktop = requests.desktops.connections.borrow().clone();
    let greeter = requests.desktops.greeter_connections.borrow().clone();
    let connections = match (linked, desktop, greeter) {
        (Some(linked), Some(connections), _) if linked.user == identity.user => connections,
        (_, _, Some(connections))
            if identity.user == crate::greeter::GREETER_USER && protocol == WAYLAND =>
        {
            connections
        }
        _ => return,
    };
    let sent = match protocol {
        WAYLAND => connections.connect(server, identity).is_ok(),
        OPENER => connections
            .connect_opener(ServerEnd::<Opener>::new(server), identity)
            .is_ok(),
        FILE_PICKER => connections
            .connect_file_picker(ServerEnd::<FilePicker>::new(server), identity)
            .is_ok(),
        _ => false,
    };
    if !sent {
        mikros_rt::log!(
            Error,
            "sessionmgr: forwarding a {protocol} connection failed"
        );
    }
}
