//! The `Broker` protocol, served over the broker's store.

use crate::permissions::{Call, Decision, Failure, Grant, Identity, Requested, Revoked, Store};
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{
    self as midl, BrokerAsyncServer, BrokerCheckResponse, BrokerGrantSystemResponse,
    BrokerListGrantsResponse, BrokerRegisterPrompterResponse, BrokerRequest, BrokerRequestRequest,
    BrokerRequestResponse, BrokerSetGrantResponse, BrokerWatchResponse, PrompterAsyncClient,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The broker's state, shared by every connection.
pub struct Broker(Rc<Inner>);

struct Inner {
    store: RefCell<Store>,
    prompter: RefCell<Option<Rc<PrompterAsyncClient>>>,
    /// The connections that watch each permission.
    watchers: RefCell<Vec<(String, Weak<BrokerAsyncServer>)>>,
    reactor: Reactor,
    spawner: Spawner,
}

impl Broker {
    pub fn new(store: Store, reactor: Reactor, spawner: Spawner) -> Broker {
        Broker(Rc::new(Inner {
            store: RefCell::new(store),
            prompter: RefCell::new(None),
            watchers: RefCell::new(Vec::new()),
            reactor,
            spawner,
        }))
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
    }
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

    /// Sends `OnRevoked` to every live connection that watches the
    /// permission, and forgets the closed ones.
    fn revoked(&self, revoked: &Revoked) {
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
    }
}

async fn serve(inner: Rc<Inner>, end: ServerEnd<midl::Broker>, caller: Rc<ClientIdentity>) {
    use BrokerRequest as R;
    let server = Rc::new(BrokerAsyncServer::new(end, inner.reactor.clone()));
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            R::Check(q, r) => {
                let result = inner.authorize(&caller, Call::Check).and_then(|()| {
                    let store = inner.store.borrow();
                    let decision = store.check(identity(&q.client), &q.permission);
                    decision.map_err(status)
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
                    crate::permissions::kind(&q.permission).ok_or(Status::NotFound)?;
                    let watcher = (q.permission, Rc::downgrade(&server));
                    inner.watchers.borrow_mut().push(watcher);
                    Ok(BrokerWatchResponse {})
                });
                r.reply(s, result)
            }
            R::SetGrant(q, r) => {
                let result = inner.authorize(&caller, Call::SetGrant).and_then(|()| {
                    let grant = Grant {
                        user: q.grant.user,
                        app_id: q.grant.app_id,
                        permission: q.grant.permission,
                        decision: from_midl(q.grant.decision)?,
                    };
                    let revoked = inner.store.borrow_mut().set_grant(&grant);
                    if let Some(revoked) = revoked.map_err(status)? {
                        inner.revoked(&revoked);
                    }
                    Ok(BrokerSetGrantResponse {})
                });
                r.reply(s, result)
            }
            R::ListGrants(q, r) => {
                let result = inner.authorize(&caller, Call::ListGrants).map(|()| {
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
                    BrokerListGrantsResponse { grants }
                });
                r.reply(s, result)
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
                let result = inner.authorize(&caller, Call::RegisterPrompter).map(|()| {
                    let prompter = PrompterAsyncClient::new(q.prompter, inner.reactor.clone());
                    *inner.prompter.borrow_mut() = Some(Rc::new(prompter));
                    BrokerRegisterPrompterResponse {}
                });
                r.reply(s, result)
            }
        };
    }
}

/// Decides `q` for `caller`, asking the prompter when the user has not
/// decided a `user` permission. Without a prompter, or when asking it
/// fails, the answer is `DENIED` and nothing is recorded; a prompter
/// that failed is forgotten.
async fn decide_request(
    inner: &Inner,
    caller: &ClientIdentity,
    q: BrokerRequestRequest,
) -> Result<BrokerRequestResponse, Status> {
    inner.authorize(caller, Call::Request)?;
    let client = identity(&q.client);
    let requested = inner.store.borrow().request(client, &q.permission);
    let decision = match requested.map_err(status)? {
        Requested::Decided(decision) => decision,
        Requested::Ask => {
            let prompter = inner.prompter.borrow().clone();
            match prompter {
                None => Decision::Denied,
                Some(prompter) => {
                    let asked = prompter.ask(
                        client.user,
                        q.client.app_id.clone(),
                        q.permission.clone(),
                        q.reason,
                    );
                    match asked.await {
                        Ok(answer) => {
                            let mut store = inner.store.borrow_mut();
                            store.record(client, &q.permission, answer.allowed)
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
