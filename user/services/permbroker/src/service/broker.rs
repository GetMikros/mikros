//! The `Broker` protocol, served over the broker's store. An `admin`
//! permission that a client does not hold is asked for with the session
//! manager's `Authorization`, through a connection of its own each time,
//! so that a restarted session manager answers the next one; without an
//! answer, it is not allowed.

use crate::permissions::{
    self as store, Call, Decision, Failure, Grant, Identity, MANAGE_GRANTS, Requested, Revoked,
    Store,
};
use crate::service::files::{self, Files};
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{
    self as midl, BrokerAsyncServer, BrokerCheckResponse, BrokerGrantSystemResponse,
    BrokerListGrantsRequest, BrokerListGrantsResponse, BrokerRegisterPrompterResponse,
    BrokerRequest, BrokerRequestRequest, BrokerRequestResponse, BrokerRevocationDoneResponse,
    BrokerSetGrantRequest, BrokerSetGrantResponse, BrokerWatchConfirmedResponse,
    BrokerWatchResponse, PrompterAsyncClient,
};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::{ClientIdentity, DirectoryAsyncClient};
use mikros_rt::interfaces::mikros::session::{Authorization, AuthorizationAsyncClient};

/// The session manager's protocol that asks an administrator.
const AUTHORIZATION: &str = "mikros.session.Authorization";
/// The only prompter of the broker.
const SESSION_MANAGER: &str = "system.sessionmgr";
/// The longest reason that `Authorize` takes, in bytes.
const REASON_MAX: usize = 256;
/// How long the services that watch a permission with confirmation have
/// to confirm a revocation, and how often the broker looks.
const CONFIRM_WAIT: i64 = 5_000_000_000;
const CONFIRM_POLL: i64 = 20_000_000;
/// The services that are only logged when they confirm late, being
/// critical; every other is killed.
const CRITICAL: [&str; 2] = ["system.processmgr", "system.servicemgr"];
const KILL: u32 = 9;
/// The most permissions one connection watches (MIKSPEC-0037.R47).
const WATCHES_MAX: usize = 256;

/// A connection that watches a permission with confirmation
/// (MIKSPEC-0037.R34), and the revocations it has confirmed.
struct Confirming {
    permission: String,
    server: Weak<BrokerAsyncServer>,
    caller: Rc<ClientIdentity>,
    done: Rc<RefCell<BTreeSet<u64>>>,
}

/// A revocation sent to a confirming connection, until it confirms.
struct Pending {
    sequence: u64,
    server: Weak<BrokerAsyncServer>,
    caller: Rc<ClientIdentity>,
    done: Rc<RefCell<BTreeSet<u64>>>,
}

fn now() -> i64 {
    mikros_rt::objects::now()
}

/// The broker's state, shared by every connection.
pub struct Broker(Rc<Inner>);

struct Inner {
    store: Rc<RefCell<Store>>,
    /// Whether the stored grants and numbers have been read, and whether
    /// the store changed since they were written.
    files: Rc<Files>,
    /// The broker's own directory, through which it reaches the session
    /// manager and the file system of its files.
    directory: Rc<DirectoryAsyncClient>,
    prompter: RefCell<Option<Rc<PrompterAsyncClient>>>,
    /// The connections that watch each permission.
    watchers: RefCell<Vec<(String, Weak<BrokerAsyncServer>)>>,
    /// Those that watch with confirmation, and the last sequence number
    /// of a revocation sent to them.
    confirming: RefCell<Vec<Confirming>>,
    sequence: Cell<u64>,
    /// Through which a service that confirmed too late is killed.
    launcher: Rc<LauncherAsyncClient>,
    reactor: Reactor,
    spawner: Spawner,
}

impl Broker {
    /// The broker of `store`, which keeps its files on the root file
    /// system, or on the temporary file system when the root cannot be
    /// written.
    pub fn new(
        store: Store,
        directory: DirectoryAsyncClient,
        launcher: LauncherAsyncClient,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Broker {
        let store = Rc::new(RefCell::new(store));
        let files = Rc::new(Files::default());
        let directory = Rc::new(directory);
        spawner.spawn(files::keep(
            store.clone(),
            files.clone(),
            directory.clone(),
            reactor.clone(),
        ));
        Broker(Rc::new(Inner {
            store,
            files,
            directory,
            prompter: RefCell::new(None),
            watchers: RefCell::new(Vec::new()),
            confirming: RefCell::new(Vec::new()),
            sequence: Cell::new(0),
            launcher: Rc::new(launcher),
            reactor,
            spawner,
        }))
    }

    /// Serves `AppNumbers` on the connection `end` of `caller` until it
    /// closes: `Number` for callers that hold `system.permissions.read`,
    /// `AppId` for every caller.
    pub fn serve_numbers(&self, end: ServerEnd<midl::AppNumbers>, caller: ClientIdentity) {
        let inner = self.0.clone();
        let spawner = inner.spawner.clone();
        spawner.spawn(async move {
            use midl::AppNumbersRequest as R;
            let server = midl::AppNumbersAsyncServer::new(end, inner.reactor.clone());
            while let Ok(request) = server.next().await {
                let s = server.server();
                let _ = match request {
                    R::Number(q, r) => {
                        // The same permission as Check: system.permissions.read.
                        let result = match inner.authorize(&caller, Call::Check) {
                            Ok(()) => {
                                inner.files.loaded().await;
                                let (number, new) = inner.store.borrow_mut().number(&q.app_id);
                                if new {
                                    inner.files.changed();
                                }
                                Ok(midl::AppNumbersNumberResponse { number })
                            }
                            Err(e) => Err(e),
                        };
                        r.reply(s, result)
                    }
                    R::AppId(q, r) => {
                        let found = inner.store.borrow().app_id(q.number).map(String::from);
                        let result = found
                            .map(|app_id| midl::AppNumbersAppIdResponse { app_id })
                            .ok_or(Status::NotFound);
                        r.reply(s, result)
                    }
                };
            }
        });
    }

    /// Serves the connection `end` of `caller` until it closes.
    pub fn serve(&self, end: ServerEnd<midl::Broker>, caller: ClientIdentity) {
        let inner = self.0.clone();
        let spawner = inner.spawner.clone();
        spawner.spawn(serve(inner, end, Rc::new(caller)));
    }
}

fn status(failure: Failure) -> Status {
    match failure {
        Failure::AccessDenied => Status::AccessDenied,
        Failure::NotFound => Status::NotFound,
        Failure::InvalidArgs => Status::InvalidArgs,
    }
}

fn identity(client: &ClientIdentity) -> Identity<'_> {
    Identity {
        user: client.user,
        app_id: &client.app_id,
        admin: client.admin,
        koid: client.koid,
        checksum: &client.portable_checksum,
    }
}

/// Logs that `permission` was allowed to `client` only because an
/// administrator elevated it.
fn log_elevated(client: &ClientIdentity, permission: &str) {
    let (pid, app_id, admin) = (client.pid, &client.app_id, client.admin);
    mikros_rt::log!(
        Info,
        "permbroker: allowed {permission} to process {pid} ({app_id}) by the elevation of user {admin}"
    );
}

fn to_midl(decision: Decision) -> midl::Decision {
    match decision {
        Decision::Undetermined => midl::Decision::Undetermined,
        Decision::Allowed => midl::Decision::Allowed,
        Decision::Denied => midl::Decision::Denied,
    }
}

fn from_midl(decision: midl::Decision) -> Result<Decision, Status> {
    match decision {
        midl::Decision::Undetermined => Ok(Decision::Undetermined),
        midl::Decision::Allowed => Ok(Decision::Allowed),
        midl::Decision::Denied => Ok(Decision::Denied),
        midl::Decision::Unknown(_) => Err(Status::InvalidArgs),
    }
}

impl Inner {
    fn authorize(&self, caller: &ClientIdentity, call: Call) -> Result<(), Status> {
        self.store
            .borrow()
            .authorize(&caller.app_id, call)
            .map_err(status)
    }

    /// Whether an administrator allows the `admin` permission
    /// `permission` to `client`, asked for with `reason` through the
    /// session manager's `Authorize`; if so, the client's process holds
    /// it from then on.
    async fn ask_administrator(
        &self,
        client: &ClientIdentity,
        via: String,
        permission: &str,
        mut reason: String,
    ) -> bool {
        let Ok((authorization, server)) = endpoints::<Authorization>() else {
            return false;
        };
        let connected = self
            .directory
            .connect(AUTHORIZATION.into(), None, server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            return false;
        }
        while reason.len() > REASON_MAX {
            reason.pop();
        }
        let authorization = AuthorizationAsyncClient::new(authorization, self.reactor.clone());
        let answer = authorization
            .authorize(client.clone(), via, permission.into(), reason)
            .await;
        let allowed = matches!(answer, Ok(Ok(a)) if a.allowed);
        if allowed {
            self.store.borrow_mut().hold(identity(client), permission);
        }
        allowed
    }

    /// Fails with `ACCESS_DENIED` unless `caller` holds `MANAGE_GRANTS`,
    /// which is asked for as a `Request` of it with `reason` would.
    async fn manage_grants(&self, caller: &ClientIdentity, reason: String) -> Result<(), Status> {
        let requested = self.store.borrow().request(identity(caller), MANAGE_GRANTS);
        match requested.map_err(status)? {
            (Requested::Decided(Decision::Allowed), elevated) => {
                if elevated {
                    log_elevated(caller, MANAGE_GRANTS);
                }
                Ok(())
            }
            (Requested::Authorize, _)
                if self
                    .ask_administrator(caller, String::new(), MANAGE_GRANTS, reason)
                    .await =>
            {
                Ok(())
            }
            _ => Err(Status::AccessDenied),
        }
    }

    /// Sends `OnRevoked` to every live connection that watches the
    /// permission, and `OnRevokedConfirmed` with a new sequence number to
    /// every one that watches it with confirmation, which it returns, and
    /// forgets the closed ones.
    fn revoked(&self, revoked: &Revoked) -> Vec<Pending> {
        self.watchers.borrow_mut().retain(|(permission, watcher)| {
            let Some(watcher) = watcher.upgrade() else {
                return false;
            };
            if *permission == revoked.permission {
                let _ = watcher.server().send_on_revoked(
                    revoked.user,
                    revoked.app_id.clone(),
                    revoked.permission.clone(),
                );
            }
            true
        });
        let mut pending = Vec::new();
        self.confirming.borrow_mut().retain(|c| {
            let Some(server) = c.server.upgrade() else {
                return false;
            };
            if c.permission == revoked.permission {
                let sequence = self.sequence.get() + 1;
                self.sequence.set(sequence);
                let _ = server.server().send_on_revoked_confirmed(
                    sequence,
                    revoked.user,
                    revoked.app_id.clone(),
                    revoked.permission.clone(),
                );
                pending.push(Pending {
                    sequence,
                    server: c.server.clone(),
                    caller: c.caller.clone(),
                    done: c.done.clone(),
                });
            }
            true
        });
        pending
    }

    /// Waits until every one of `pending` has confirmed its revocation or
    /// closed its connection, at most `CONFIRM_WAIT`; then logs each that
    /// did not, and kills its process unless it is critical
    /// (MIKSPEC-0037.R34).
    async fn confirmed(&self, mut pending: Vec<Pending>) {
        let deadline = now() + CONFIRM_WAIT;
        loop {
            pending.retain(|p| {
                let done = p.done.borrow_mut().remove(&p.sequence);
                !done && p.server.upgrade().is_some()
            });
            if pending.is_empty() || now() >= deadline {
                break;
            }
            let next = (now() + CONFIRM_POLL).min(deadline);
            if self.reactor.sleep_until(next).await.is_err() {
                break;
            }
        }
        for p in pending {
            let (pid, app_id) = (p.caller.pid, &p.caller.app_id);
            mikros_rt::log!(
                Error,
                "permbroker: process {pid} ({app_id}) did not confirm revocation {} within 5 s",
                p.sequence
            );
            // Only the process that was too late, not one that got its
            // ID since (MIKSPEC-0034.R48).
            if !CRITICAL.contains(&app_id.as_str()) {
                let _ = self.launcher.kill_checked(pid, p.caller.koid, KILL).await;
            }
        }
    }
}

async fn serve(inner: Rc<Inner>, end: ServerEnd<midl::Broker>, caller: Rc<ClientIdentity>) {
    use BrokerRequest as R;
    let server = Rc::new(BrokerAsyncServer::new(end, inner.reactor.clone()));
    // The revocations that this connection has confirmed.
    let done = Rc::new(RefCell::new(BTreeSet::new()));
    // The permissions it watches, with and without confirmation
    // (MIKSPEC-0037.R47).
    let watches = Cell::new(0usize);
    let watch = || match watches.get() < WATCHES_MAX {
        true => {
            watches.set(watches.get() + 1);
            Ok(())
        }
        false => Err(Status::NoResources),
    };
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            R::Check(q, r) => {
                let result = inner.authorize(&caller, Call::Check).and_then(|()| {
                    let store = inner.store.borrow();
                    let (decision, elevated) = store
                        .check(identity(&q.client), &q.permission)
                        .map_err(status)?;
                    if elevated {
                        log_elevated(&q.client, &q.permission);
                    }
                    Ok(decision)
                });
                r.reply(
                    s,
                    result.map(|d| BrokerCheckResponse {
                        decision: to_midl(d),
                    }),
                )
            }
            R::Request(q, r) => {
                // A prompt may take long, so the request runs on its own.
                let (inner, server, caller) = (inner.clone(), server.clone(), caller.clone());
                inner.spawner.clone().spawn(async move {
                    let result = decide_request(&inner, &caller, q).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::Watch(q, r) => {
                let result = inner.authorize(&caller, Call::Watch).and_then(|()| {
                    crate::permissions::scope(&q.permission).ok_or(Status::NotFound)?;
                    watch()?;
                    let watcher = (q.permission, Rc::downgrade(&server));
                    inner.watchers.borrow_mut().push(watcher);
                    Ok(BrokerWatchResponse {})
                });
                r.reply(s, result)
            }
            R::WatchConfirmed(q, r) => {
                let result = inner.authorize(&caller, Call::Watch).and_then(|()| {
                    crate::permissions::scope(&q.permission).ok_or(Status::NotFound)?;
                    watch()?;
                    inner.confirming.borrow_mut().push(Confirming {
                        permission: q.permission,
                        server: Rc::downgrade(&server),
                        caller: caller.clone(),
                        done: done.clone(),
                    });
                    Ok(BrokerWatchConfirmedResponse {})
                });
                r.reply(s, result)
            }
            R::RevocationDone(q, r) => {
                done.borrow_mut().insert(q.sequence);
                r.reply(s, BrokerRevocationDoneResponse {})
            }
            R::SetGrant(q, r) => {
                // Asking an administrator may take long, so the call runs
                // on its own.
                let (inner, server, caller) = (inner.clone(), server.clone(), caller.clone());
                inner.spawner.clone().spawn(async move {
                    let result = set_grant(&inner, &caller, q).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::ListGrants(q, r) => {
                let (inner, server, caller) = (inner.clone(), server.clone(), caller.clone());
                inner.spawner.clone().spawn(async move {
                    let result = list_grants(&inner, &caller, q).await;
                    let _ = r.reply(server.server(), result);
                });
                Ok(())
            }
            R::GrantSystem(q, r) => {
                let result = inner.authorize(&caller, Call::GrantSystem).and_then(|()| {
                    let mut store = inner.store.borrow_mut();
                    let granted = store.grant_system(&q.app_id, &q.permissions);
                    granted
                        .map(|()| BrokerGrantSystemResponse {})
                        .map_err(status)
                });
                r.reply(s, result)
            }
            R::RegisterPrompter(q, r) => {
                // Only the session manager asks users (MIKSPEC-0037.R42).
                let only = match caller.app_id == SESSION_MANAGER {
                    true => Ok(()),
                    false => Err(Status::AccessDenied),
                };
                let result = only
                    .and_then(|()| inner.authorize(&caller, Call::RegisterPrompter))
                    .map(|()| {
                        let prompter = PrompterAsyncClient::new(q.prompter, inner.reactor.clone());
                        *inner.prompter.borrow_mut() = Some(Rc::new(prompter));
                        BrokerRegisterPrompterResponse {}
                    });
                r.reply(s, result)
            }
        };
    }
}

/// Sets the grant of `q` for `caller`, once an administrator allowed
/// `MANAGE_GRANTS` when it is for another user or `ALLOWED`. A grant the
/// store would refuse fails before anyone is asked.
async fn set_grant(
    inner: &Inner,
    caller: &ClientIdentity,
    q: BrokerSetGrantRequest,
) -> Result<BrokerSetGrantResponse, Status> {
    inner.authorize(caller, Call::SetGrant)?;
    let grant = Grant {
        user: q.grant.user,
        app_id: q.grant.app_id,
        permission: q.grant.permission,
        decision: from_midl(q.grant.decision)?,
    };
    Store::check_grant(&grant).map_err(status)?;
    if store::setting_needs_manage(identity(caller), &grant) {
        let verb = match grant.decision {
            Decision::Allowed => "allow",
            _ => "deny",
        };
        let (permission, app_id) = (&grant.permission, &grant.app_id);
        let reason = match grant.user == caller.user {
            true => format!("{verb} {permission} for {app_id}"),
            false => format!("{verb} {permission} for {app_id} for user {}", grant.user),
        };
        inner.manage_grants(caller, reason).await?;
    }
    let revoked = inner.store.borrow_mut().set_grant(&grant);
    inner.files.changed();
    // Only once those that watch with confirmation have stopped access
    // (MIKSPEC-0037.R34).
    if let Some(revoked) = revoked.map_err(status)? {
        let pending = inner.revoked(&revoked);
        inner.confirmed(pending).await;
    }
    Ok(BrokerSetGrantResponse {})
}

/// The grants of the user of `q`, once an administrator allowed
/// `MANAGE_GRANTS` when it is not `caller`'s.
async fn list_grants(
    inner: &Inner,
    caller: &ClientIdentity,
    q: BrokerListGrantsRequest,
) -> Result<BrokerListGrantsResponse, Status> {
    inner.authorize(caller, Call::ListGrants)?;
    if store::listing_needs_manage(identity(caller), q.user) {
        let reason = format!("list the grants of user {}", q.user);
        inner.manage_grants(caller, reason).await?;
    }
    let grants = inner.store.borrow().list(q.user);
    let grants = grants
        .into_iter()
        .map(|g| midl::Grant {
            user: g.user,
            app_id: g.app_id,
            permission: g.permission,
            decision: to_midl(g.decision),
        })
        .collect();
    Ok(BrokerListGrantsResponse { grants })
}

/// Decides `q` for `caller`, asking the prompter when the user has not
/// decided a `user` permission, and the session manager when the client
/// does not hold an `admin` one. Without a prompter, or when asking it
/// fails, the answer is `DENIED` and nothing is recorded; a prompter
/// that failed is forgotten. An `admin` permission is `ALLOWED` only when
/// an administrator allowed it.
async fn decide_request(
    inner: &Inner,
    caller: &ClientIdentity,
    q: BrokerRequestRequest,
) -> Result<BrokerRequestResponse, Status> {
    inner.authorize(caller, Call::Request)?;
    // The service that asks for a permission of one of its clients, which
    // a question names besides the client (MIKSPEC-0037.R46).
    let via = match caller.koid == q.client.koid {
        true => String::new(),
        false => caller.app_id.clone(),
    };
    let client = identity(&q.client);
    let requested = inner.store.borrow().request(client, &q.permission);
    let (requested, elevated) = requested.map_err(status)?;
    if elevated {
        log_elevated(&q.client, &q.permission);
    }
    let decision = match requested {
        Requested::Decided(decision) => decision,
        Requested::Authorize => {
            match inner
                .ask_administrator(&q.client, via, &q.permission, q.reason)
                .await
            {
                true => Decision::Allowed,
                false => Decision::Denied,
            }
        }
        Requested::Ask => {
            let prompter = inner.prompter.borrow().clone();
            match prompter {
                None => Decision::Denied,
                Some(prompter) => {
                    let asked = prompter.ask(q.client.clone(), via, q.permission.clone(), q.reason);
                    match asked.await {
                        // Nobody answered: denied, and nothing recorded
                        // (MIKSPEC-0037.R41).
                        Ok(answer) if answer.decision == midl::Decision::Undetermined => {
                            Decision::Denied
                        }
                        Ok(answer) => {
                            let allowed = answer.decision == midl::Decision::Allowed;
                            let mut store = inner.store.borrow_mut();
                            let decision = store.record(client, &q.permission, allowed);
                            inner.files.changed();
                            decision
                        }
                        Err(_) => {
                            let mut current = inner.prompter.borrow_mut();
                            if current.as_ref().is_some_and(|p| Rc::ptr_eq(p, &prompter)) {
                                *current = None;
                            }
                            Decision::Denied
                        }
                    }
                }
            }
        }
    };
    Ok(BrokerRequestResponse {
        decision: to_midl(decision),
    })
}
