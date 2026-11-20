//! Administrator authorization and user questions:
//! `mikros.session.Authorization`, which the session manager serves to
//! the permission broker alone, for the clients that ask the broker for
//! `admin` permissions, and `mikros.permissions.Prompter`, with which the
//! broker asks it for `user` permissions (MIKSPEC-0057.R30). It asks
//! through the authorization screen of the text console that the
//! client's login session runs on, with `OnAuthorize` or `OnAsk`, one
//! request per console at a time, and ends the request on `Answer`,
//! `AnswerAsk` or `Cancel`, after 60 seconds, or when the client
//! terminates (MIKSPEC-0057.R33).

use crate::wait::{Either, Slot, Waiters, select};
use crate::{SECOND, now};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{
    Broker, BrokerAsyncClient, Decision, Prompter, PrompterAskResponse, PrompterAsyncServer,
    PrompterRequest,
};
use mikros_rt::interfaces::mikros::process::{
    ElevationsAsyncClient, LauncherAsyncClient, ProcessInfo,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{
    AccountsAsyncClient, Authorization, AuthorizationAsyncServer, AuthorizationAuthorizeResponse,
    AuthorizationRequest, TextLoginAsyncServer, User, UserKind,
};

/// How long a request waits for an answer, and how often its task looks
/// whether the client still runs, which it has no handle to wait on.
const BROKER: &str = "mikros.permissions.Broker";
const TIMEOUT: i64 = 60 * SECOND;
const CLIENT_POLL: i64 = SECOND;

/// How an open request ended, for its `Authorize`.
enum Outcome {
    /// `Answer` checked the credentials: the administrator's user ID, or
    /// none when they were wrong or not an administrator's.
    Answered(Option<u32>),
    /// `AnswerAsk`: whether the user allowed it.
    Asked(bool),
    Cancelled,
}

/// An open request: its console, and the slot of its outcome, which its
/// task waits on.
struct Open {
    console: u8,
    outcome: Rc<Slot<Outcome>>,
    /// Whether it is an `OnAsk`, which `AnswerAsk` answers, rather than
    /// an `OnAuthorize`, which `Answer` does.
    ask: bool,
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
    /// The sessions handed to consoles that have not started them yet.
    pub handovers: RefCell<BTreeMap<u8, crate::login::Handover>>,
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
            handovers: RefCell::new(BTreeMap::new()),
            open: RefCell::new(BTreeMap::new()),
            freed: Waiters::default(),
            next: Cell::new(1),
        }
    }

    /// Checks `name` and `password` for open request `request`, once:
    /// fails with `NOT_FOUND` for a request that is not open, and with
    /// `ACCESS_DENIED` unless they are an administrator's.
    pub async fn answer(&self, request: u32, name: String, password: String) -> Result<(), Status> {
        if !self.open.borrow().get(&request).is_some_and(|o| !o.ask) {
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

    /// Answers open `OnAsk` request `request` with `allowed`, once: fails
    /// with `NOT_FOUND` for one that is not open or not an `OnAsk`.
    pub fn answer_ask(&self, request: u32, allowed: bool) -> Result<(), Status> {
        if !self.open.borrow().get(&request).is_some_and(|o| o.ask) {
            return Err(Status::NotFound);
        }
        self.give(request, Outcome::Asked(allowed))
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

    /// The administrator who elevated the process with the kernel object
    /// ID `koid`, 0 for none.
    pub async fn admin_of(&self, koid: u64) -> u32 {
        let elevations = self.elevations.borrow().clone();
        match elevations {
            Some(e) => e
                .get_admin(koid)
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
        via: String,
        action: String,
        reason: String,
    ) -> Option<(bool, u32)> {
        let admin = self.admin_of(client.koid).await;
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
        self.ask(client, via, action, reason).await
    }

    /// The text console of the client's login session, and the
    /// `TextLogin` server to ask there, or none when it runs in no text
    /// console's session or when the process that the launcher describes
    /// is not the one that `client` names: its kernel object ID, user, app
    /// ID and portable checksum must all be the client's, so that a
    /// question names the app of the process that holds the answer
    /// (MIKSPEC-0057.R33).
    async fn console_of(
        &self,
        client: &ClientIdentity,
        what: &str,
    ) -> Option<(u8, Rc<TextLoginAsyncServer>)> {
        let pid = client.pid;
        let info = match self.launcher.get_info(pid).await {
            Ok(Ok(info)) => info.info,
            other => {
                mikros_rt::log!(Error, "sessionmgr: {what}: describing {pid}: {other:?}");
                return None;
            }
        };
        if !is_client(&info, client) {
            mikros_rt::log!(
                Warning,
                "sessionmgr: {what}: process {pid} is not the client that was named; refused"
            );
            return None;
        }
        let console = self
            .sessions
            .borrow()
            .iter()
            .find(|&(_, &s)| s != 0 && s == info.login_session)
            .map(|(&c, _)| c);
        let (Some(console), Some(server)) = (console, self.text_login.borrow().clone()) else {
            mikros_rt::log!(
                Info,
                "sessionmgr: {what}: process {pid} runs on no text console; refused"
            );
            return None;
        };
        Some((console, server))
    }

    /// Opens a request on `console` of the kind `ask`, once the console
    /// has no other open request, and its number and the slot of its
    /// outcome.
    async fn open(&self, console: u8, ask: bool) -> (u32, Rc<Slot<Outcome>>) {
        // One request per console: the next starts once it is free.
        self.freed.until(|| !self.busy(console)).await;
        let request = self.next.get();
        self.next.set(request.wrapping_add(1).max(1));
        let slot = Rc::new(Slot::default());
        let open = Open {
            console,
            outcome: slot.clone(),
            ask,
        };
        self.open.borrow_mut().insert(request, open);
        (request, slot)
    }

    /// The outcome of open request `request` once it was sent with
    /// `sent`: none after 60 seconds, and `Err` when the client
    /// terminated first. The request is closed, and the console told
    /// that it ended unless it ended there.
    async fn wait(
        &self,
        client: &ClientIdentity,
        request: u32,
        slot: &Slot<Outcome>,
        server: &TextLoginAsyncServer,
        sent: bool,
    ) -> Result<Option<Outcome>, ()> {
        let deadline = now() + TIMEOUT;
        let outcome = loop {
            if !sent {
                break None;
            }
            // The outcome, or the next look at the client, or the end.
            let next = now().saturating_add(CLIENT_POLL).min(deadline);
            match select(slot.take(), self.reactor.sleep_until(next)).await {
                Either::Left(outcome) => break Some(outcome),
                Either::Right(_) if now() >= deadline => break None,
                Either::Right(_) => {
                    // Its ID may have gone to another process since
                    // (MIKSPEC-0034.R48).
                    let info = self.launcher.get_info(client.pid).await;
                    if !matches!(info, Ok(Ok(r)) if r.info.koid == client.koid) {
                        self.close(request);
                        let _ = server.server().send_on_authorize_ended(request);
                        return Err(());
                    }
                }
            }
        };
        self.close(request);
        if outcome.is_none() {
            let _ = server.server().send_on_authorize_ended(request);
        }
        Ok(outcome)
    }

    /// Asks on the client's console for `action`, as `authorize` does for
    /// a process that is not elevated.
    /// `via` is the service that asks for its client, or empty
    /// (MIKSPEC-0057.R35).
    async fn ask(
        &self,
        client: &ClientIdentity,
        via: String,
        action: String,
        reason: String,
    ) -> Option<(bool, u32)> {
        let denied = Some((false, 0));
        let Some((console, server)) = self.console_of(client, "Authorize").await else {
            return denied;
        };
        let name = match user(&self.accounts, client.user).await {
            Some(u) if u.kind == UserKind::Admin => u.name,
            _ => String::new(),
        };
        let (request, slot) = self.open(console, false).await;
        let app_id = client.app_id.clone();
        let sent = server
            .server()
            .send_on_authorize(request, console, app_id, via, action, reason, name);
        match self
            .wait(client, request, &slot, &server, sent.is_ok())
            .await
        {
            Err(()) => None,
            Ok(Some(Outcome::Answered(Some(admin)))) => Some((true, admin)),
            Ok(_) => denied,
        }
    }

    /// `Ask` of the broker for the `user` permission `permission` of
    /// `client`: asked as a `[y/N]` line on the client's text console, and
    /// `UNDETERMINED` when nobody answers there or it runs in no text
    /// console's session (MIKSPEC-0057.R33).
    pub async fn ask_permission(
        &self,
        client: &ClientIdentity,
        via: String,
        permission: String,
        reason: String,
    ) -> Decision {
        let Some((console, server)) = self.console_of(client, "Ask").await else {
            return Decision::Undetermined;
        };
        let (request, slot) = self.open(console, true).await;
        let app_id = client.app_id.clone();
        let sent = server
            .server()
            .send_on_ask(request, console, app_id, via, permission, reason);
        match self
            .wait(client, request, &slot, &server, sent.is_ok())
            .await
        {
            Ok(Some(Outcome::Asked(true))) => Decision::Allowed,
            Ok(Some(Outcome::Asked(false))) => Decision::Denied,
            _ => Decision::Undetermined,
        }
    }
}

/// Whether `info`, as the launcher describes the process of `client`'s
/// process ID, is the process that `client` names (MIKSPEC-0057.R33).
fn is_client(info: &ProcessInfo, client: &ClientIdentity) -> bool {
    info.koid == client.koid
        && info.user == client.user
        && info.app_id == client.app_id
        && info.portable_checksum == client.portable_checksum
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
            let answer = requests
                .authorize(&q.client, q.via, q.action, q.reason)
                .await;
            if let Some((allowed, admin)) = answer {
                let reply = AuthorizationAuthorizeResponse { allowed, admin };
                let _ = r.reply(server.server(), Ok(reply));
            }
        });
    }
}

/// Serves `Prompter` to the broker on `end`, each `Ask` in a task of its
/// own (MIKSPEC-0057.R30).
pub async fn serve_prompter(requests: Rc<Requests>, end: ServerEnd<Prompter>, spawner: Spawner) {
    let server = Rc::new(PrompterAsyncServer::new(end, requests.reactor.clone()));
    while let Ok(PrompterRequest::Ask(q, r)) = server.next().await {
        let (requests, server) = (requests.clone(), server.clone());
        spawner.spawn(async move {
            let decision = requests
                .ask_permission(&q.client, q.via, q.permission, q.reason)
                .await;
            let _ = r.reply(server.server(), PrompterAskResponse { decision });
        });
    }
}

/// Registers as the broker's prompter, connected through `directory`, and
/// serves its `Ask`s.
pub async fn register_prompter(
    directory: &DirectoryAsyncClient,
    requests: Rc<Requests>,
    spawner: Spawner,
) {
    let (Ok((broker, server)), Ok((prompter, prompts))) =
        (endpoints::<Broker>(), endpoints::<Prompter>())
    else {
        return;
    };
    let connected = directory
        .connect(BROKER.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        mikros_rt::log!(Error, "sessionmgr: no broker: {connected:?}");
        return;
    }
    let broker = BrokerAsyncClient::new(broker, requests.reactor.clone());
    match broker.register_prompter(prompter).await {
        Ok(Ok(_)) => serve_prompter(requests, prompts, spawner).await,
        other => mikros_rt::log!(Error, "sessionmgr: RegisterPrompter: {other:?}"),
    }
}
