//! Administrator authorization: `mikros.session.Authorization`, which the
//! session manager serves to processes with `session.authorize`. It asks
//! through the authorization screen of the text console that the
//! client's login session runs on, with `OnAuthorize`, one request per
//! console at a time, and ends the request on `Answer` or `Cancel`, after
//! 60 seconds, or when the client terminates.

use crate::wait::{Either, Slot, Waiters, select};
use crate::{SECOND, now};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{ElevationsAsyncClient, LauncherAsyncClient};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::session::{
    AccountsAsyncClient, Authorization, AuthorizationAsyncServer, AuthorizationAuthorizeResponse,
    AuthorizationRequest, TextLoginAsyncServer, User, UserKind,
};

/// How long a request waits for an answer, and how often its task looks
/// whether the client still runs, which it has no handle to wait on.
const TIMEOUT: i64 = 60 * SECOND;
const CLIENT_POLL: i64 = SECOND;

/// How an open request ended, for its `Authorize`.
enum Outcome {
    /// `Answer` checked the credentials: the administrator's user ID, or
    /// none when they were wrong or not an administrator's.
    Answered(Option<u32>),
    Cancelled,
}

/// An open request: its console, and the slot of its outcome, which its
/// task waits on.
struct Open {
    console: u8,
    outcome: Rc<Slot<Outcome>>,
}

/// The requests and what they need.
pub struct Requests {
    pub accounts: Rc<AccountsAsyncClient>,
    pub launcher: Rc<LauncherAsyncClient>,
    pub reactor: Reactor,
    /// The `TextLogin` server, once the console connected.
    pub text_login: RefCell<Option<Rc<TextLoginAsyncServer>>>,
    /// The process manager's `Elevations`, once it is open.
    pub elevations: RefCell<Option<Rc<ElevationsAsyncClient>>>,
    /// The login session of the session on each console.
    pub sessions: RefCell<BTreeMap<u8, u32>>,
    /// The open requests by number.
    open: RefCell<BTreeMap<u32, Open>>,
    /// The tasks that wait for a console's open request to end.
    freed: Waiters,
    next: Cell<u32>,
}

impl Requests {
    pub fn new(
        accounts: Rc<AccountsAsyncClient>,
        launcher: Rc<LauncherAsyncClient>,
        reactor: Reactor,
    ) -> Requests {
        Requests {
            accounts,
            launcher,
            reactor,
            text_login: RefCell::new(None),
            elevations: RefCell::new(None),
            sessions: RefCell::new(BTreeMap::new()),
            open: RefCell::new(BTreeMap::new()),
            freed: Waiters::default(),
            next: Cell::new(1),
        }
    }

    /// Checks `name` and `password` for open request `request`, once:
    /// fails with `NOT_FOUND` for a request that is not open, and with
    /// `ACCESS_DENIED` unless they are an administrator's.
    pub async fn answer(&self, request: u32, name: String, password: String) -> Result<(), Status> {
        if !self.open.borrow().contains_key(&request) {
            return Err(Status::NotFound);
        }
        let admin = match self.accounts.authenticate(name, password).await {
            Ok(Ok(a)) => user(&self.accounts, a.user)
                .await
                .filter(|u| u.kind == UserKind::Admin)
                .map(|u| u.id),
            _ => None,
        };
        // The request may have ended while the password was checked.
        self.give(request, Outcome::Answered(admin))?;
        admin.map(drop).ok_or(Status::AccessDenied)
    }

    /// Ends open request `request` without an answer.
    pub fn cancel(&self, request: u32) -> Result<(), Status> {
        self.give(request, Outcome::Cancelled)
    }

    /// Gives open request `request` its outcome, once.
    fn give(&self, request: u32, outcome: Outcome) -> Result<(), Status> {
        let slot = self.open.borrow().get(&request).map(|o| o.outcome.clone());
        match slot {
            Some(slot) if slot.give(outcome) => Ok(()),
            _ => Err(Status::NotFound),
        }
    }

    /// Closes request `request`, and wakes the requests that wait for its
    /// console.
    fn close(&self, request: u32) {
        self.open.borrow_mut().remove(&request);
        self.freed.wake_all();
    }

    /// Whether `console` has an open request.
    fn busy(&self, console: u8) -> bool {
        self.open.borrow().values().any(|o| o.console == console)
    }

    /// The administrator who elevated process `pid`, 0 for none.
    pub async fn admin_of(&self, pid: u32) -> u32 {
        let elevations = self.elevations.borrow().clone();
        match elevations {
            Some(e) => e
                .get_admin(pid)
                .await
                .ok()
                .and_then(Result::ok)
                .map_or(0, |r| r.admin),
            None => 0,
        }
    }

    /// `Authorize` of `client` for `action`: allowed without asking for
    /// a process that an administrator elevated while that user is still
    /// one, and otherwise whether an administrator allowed it on the
    /// client's console, and who, or `None` when the client terminated
    /// before the request ended.
    pub async fn authorize(
        &self,
        client: &ClientIdentity,
        action: String,
        reason: String,
    ) -> Option<(bool, u32)> {
        let admin = self.admin_of(client.pid).await;
        let still = admin != 0
            && user(&self.accounts, admin)
                .await
                .is_some_and(|u| u.kind == UserKind::Admin);
        if still {
            let pid = client.pid;
            mikros_rt::log!(
                Info,
                "sessionmgr: Authorize of process {pid} for {action}: allowed by the elevation of user {admin}"
            );
            return Some((true, admin));
        }
        self.ask(client, action, reason).await
    }

    /// Asks on the client's console for `action`, as `authorize` does for
    /// a process that is not elevated.
    async fn ask(
        &self,
        client: &ClientIdentity,
        action: String,
        reason: String,
    ) -> Option<(bool, u32)> {
        let denied = Some((false, 0));
        let info = match self.launcher.get_info(client.pid).await {
            Ok(Ok(info)) => info,
            other => {
                let pid = client.pid;
                mikros_rt::log!(Error, "sessionmgr: Authorize: describing {pid}: {other:?}");
                return denied;
            }
        };
        let session = info.info.login_session;
        let console = self
            .sessions
            .borrow()
            .iter()
            .find(|&(_, &s)| s != 0 && s == session)
            .map(|(&c, _)| c);
        let (Some(console), Some(server)) = (console, self.text_login.borrow().clone()) else {
            let pid = client.pid;
            mikros_rt::log!(
                Info,
                "sessionmgr: Authorize: process {pid} runs on no text console; refused"
            );
            return denied;
        };
        let name = match user(&self.accounts, client.user).await {
            Some(u) if u.kind == UserKind::Admin => u.name,
            _ => String::new(),
        };
        // One request per console: the next starts once it is free.
        self.freed.until(|| !self.busy(console)).await;
        let request = self.next.get();
        self.next.set(request.wrapping_add(1).max(1));
        let slot = Rc::new(Slot::default());
        let open = Open {
            console,
            outcome: slot.clone(),
        };
        self.open.borrow_mut().insert(request, open);
        let app_id = client.app_id.clone();
        let sent = server
            .server()
            .send_on_authorize(request, console, app_id, action, reason, name);
        let deadline = now() + TIMEOUT;
        let outcome = loop {
            if sent.is_err() {
                break None;
            }
            // The outcome, or the next look at the client, or the end.
            let next = now().saturating_add(CLIENT_POLL).min(deadline);
            match select(slot.take(), self.reactor.sleep_until(next)).await {
                Either::Left(outcome) => break Some(outcome),
                Either::Right(_) if now() >= deadline => break None,
                Either::Right(_) => {
                    if !matches!(self.launcher.get_info(client.pid).await, Ok(Ok(_))) {
                        self.close(request);
                        let _ = server.server().send_on_authorize_ended(request);
                        return None;
                    }
                }
            }
        };
        self.close(request);
        match outcome {
            Some(Outcome::Answered(Some(admin))) => Some((true, admin)),
            Some(_) => denied,
            None => {
                let _ = server.server().send_on_authorize_ended(request);
                denied
            }
        }
    }
}

/// The account of user `id`, when there is one.
async fn user(accounts: &AccountsAsyncClient, id: u32) -> Option<User> {
    let mut after = 0;
    loop {
        let users = accounts.list_users(after).await.ok()?.users;
        let last = users.last()?.id;
        if let Some(user) = users.into_iter().find(|u| u.id == id) {
            return Some(user);
        }
        after = last;
    }
}

/// Serves `Authorization` on `end`, each `Authorize` in a task of its
/// own, which replies nothing when the client terminated meanwhile.
pub async fn serve(requests: Rc<Requests>, end: ServerEnd<Authorization>, spawner: Spawner) {
    let server = Rc::new(AuthorizationAsyncServer::new(end, requests.reactor.clone()));
    while let Ok(AuthorizationRequest::Authorize(q, r)) = server.next().await {
        let (requests, server) = (requests.clone(), server.clone());
        spawner.spawn(async move {
            let answer = requests.authorize(&q.client, q.action, q.reason).await;
            if let Some((allowed, admin)) = answer {
                let reply = AuthorizationAuthorizeResponse { allowed, admin };
                let _ = r.reply(server.server(), Ok(reply));
            }
        });
    }
}
