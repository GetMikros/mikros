//! The service directory: `Directory` on every process's bound
//! connection, forwarding connections to the services that provide
//! each protocol.
//!
//! A protocol that a manifest provides has one provider and no
//! instance names; connections to it wait while its service is not
//! running yet, and fail once it will not run. Named instances are kept
//! apart, for the registrar. When a protocol or instance has a
//! permission, the caller must hold it, as the broker decides; without
//! an answer from the broker, it does not.

use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_manifest::Service;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, DirectoryAsyncServer, DirectoryConnectResponse,
    DirectoryListResponse, DirectoryRequest, DirectoryWatchResponse, ProviderClient,
};

/// The directory's state, shared by every connection.
pub struct Dir {
    reactor: Reactor,
    spawner: Spawner,
    /// Whether the manifests have been read; requests wait until then.
    ready: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
    broker: RefCell<Option<Rc<BrokerAsyncClient>>>,
    /// The protocols that manifests provide.
    provided: RefCell<BTreeMap<String, Provided>>,
    /// The named instances of each protocol.
    instances: RefCell<BTreeMap<String, BTreeMap<String, Instance>>>,
    /// The connections that watch each protocol.
    watchers: RefCell<Vec<Watcher>>,
}

/// A protocol that a manifest provides.
struct Provided {
    /// The service that provides it.
    service: String,
    permission: Option<String>,
    provider: Provider,
}

enum Provider {
    /// The service is not running yet; these connections wait for it.
    Waiting(Vec<(OwnedHandle, ClientIdentity)>),
    Running(Rc<ProviderClient>),
    /// The service will not run.
    Never,
}

/// A named instance of a protocol.
#[expect(dead_code, reason = "made by the registrar")]
pub struct Instance {
    pub permission: Option<String>,
    pub provider: ProviderClient,
    /// The pid of the process that registered it.
    pub owner: u32,
}

#[expect(dead_code, reason = "told of instances by the registrar")]
struct Watcher {
    protocol: String,
    caller: Rc<ClientIdentity>,
    server: Weak<DirectoryAsyncServer>,
}

impl Dir {
    pub fn new(reactor: Reactor, spawner: Spawner) -> Rc<Dir> {
        Rc::new(Dir {
            reactor,
            spawner,
            ready: Cell::new(false),
            waiting: RefCell::new(Vec::new()),
            broker: RefCell::new(None),
            provided: RefCell::new(BTreeMap::new()),
            instances: RefCell::new(BTreeMap::new()),
            watchers: RefCell::new(Vec::new()),
        })
    }

    /// Serves `Directory` on `end` for `caller` until it closes.
    pub fn serve(self: &Rc<Self>, end: ServerEnd<Directory>, caller: ClientIdentity) {
        let dir = self.clone();
        self.spawner.spawn(serve(dir, end, Rc::new(caller)));
    }

    /// Records the protocols that `services` provide, where `runs` says
    /// whether each service may still run, and lets requests through.
    /// A protocol that two services provide is left unprovided.
    pub fn set_services(&self, services: &[Service], runs: impl Fn(usize) -> bool) {
        let mut providers = BTreeMap::<String, Vec<usize>>::new();
        for (i, s) in services.iter().enumerate() {
            for p in &s.provides {
                providers.entry(p.protocol.clone()).or_default().push(i);
            }
        }
        let mut provided = self.provided.borrow_mut();
        for (protocol, by) in providers {
            let [i] = by[..] else {
                mikros_rt::log!(
                    Error,
                    "protocol {protocol} is provided by more than one service, so by none"
                );
                continue;
            };
            let s = &services[i];
            let permission = s
                .provides
                .iter()
                .find(|p| p.protocol == protocol)
                .and_then(|p| p.permission.clone());
            let provider = if runs(i) {
                Provider::Waiting(Vec::new())
            } else {
                Provider::Never
            };
            let service = s.name.clone();
            provided.insert(
                protocol,
                Provided {
                    service,
                    permission,
                    provider,
                },
            );
        }
        drop(provided);
        self.ready.set(true);
        for waker in self.waiting.take() {
            waker.wake();
        }
    }

    pub fn spawner(&self) -> &Spawner {
        &self.spawner
    }

    /// Uses `broker` for permission checks.
    pub fn set_broker(&self, broker: Rc<BrokerAsyncClient>) {
        *self.broker.borrow_mut() = Some(broker);
    }

    /// Forwards the connections to the protocols of `service`, now and
    /// from now on, to `provider`.
    pub fn running(&self, service: &str, provider: Rc<ProviderClient>) {
        for (protocol, p) in self.provided.borrow_mut().iter_mut() {
            if p.service != service {
                continue;
            }
            let before = core::mem::replace(&mut p.provider, Provider::Running(provider.clone()));
            if let Provider::Waiting(waiting) = before {
                for (server, client) in waiting {
                    let _ = provider.connect(protocol.clone(), None, server, client);
                }
            }
        }
    }

    /// Fails the connections to the protocols of `service`, which will
    /// not run, by closing them.
    pub fn never(&self, service: &str) {
        for p in self.provided.borrow_mut().values_mut() {
            if p.service == service {
                p.provider = Provider::Never;
            }
        }
    }

    async fn wait_ready(&self) {
        poll_fn(|cx| {
            if self.ready.get() {
                Poll::Ready(())
            } else {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Whether `caller` holds `permission`, if there is one.
    async fn allowed(&self, caller: &ClientIdentity, permission: Option<&str>) -> bool {
        let Some(permission) = permission else {
            return true;
        };
        let broker = self.broker.borrow().clone();
        let Some(broker) = broker else {
            return false;
        };
        let checked = broker.check(clone(caller), permission.into()).await;
        matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }

    async fn connect(
        &self,
        caller: &ClientIdentity,
        protocol: String,
        instance: Option<String>,
        server: OwnedHandle,
    ) -> Result<(), Status> {
        let permission = {
            let provided = self.provided.borrow();
            if let Some(p) = provided.get(&protocol) {
                if instance.is_some() {
                    return Err(Status::InvalidArgs);
                }
                if matches!(p.provider, Provider::Never) {
                    return Err(Status::NotFound);
                }
                p.permission.clone()
            } else {
                let instances = self.instances.borrow();
                let named = instances.get(&protocol).filter(|i| !i.is_empty());
                let named = named.ok_or(Status::NotFound)?;
                let name = instance.as_ref().ok_or(Status::InvalidArgs)?;
                named.get(name).ok_or(Status::NotFound)?.permission.clone()
            }
        };
        if !self.allowed(caller, permission.as_deref()).await {
            return Err(Status::AccessDenied);
        }
        // The provider may have changed while the broker answered.
        if let Some(p) = self.provided.borrow_mut().get_mut(&protocol) {
            return match &mut p.provider {
                Provider::Waiting(waiting) => {
                    waiting.push((server, clone(caller)));
                    Ok(())
                }
                Provider::Running(provider) => {
                    let _ = provider.connect(protocol, None, server, clone(caller));
                    Ok(())
                }
                Provider::Never => Err(Status::NotFound),
            };
        }
        let instances = self.instances.borrow();
        let found = instances
            .get(&protocol)
            .zip(instance.as_ref())
            .and_then(|(named, name)| named.get(name));
        let instance_ = found.ok_or(Status::NotFound)?;
        let _ = instance_
            .provider
            .connect(protocol, instance, server, clone(caller));
        Ok(())
    }

    /// The named instances of `protocol` that `caller` may connect to.
    async fn list(&self, caller: &ClientIdentity, protocol: &str) -> Result<Vec<String>, Status> {
        if self.provided.borrow().contains_key(protocol) {
            return Err(Status::InvalidArgs);
        }
        let named: Vec<(String, Option<String>)> = {
            let instances = self.instances.borrow();
            let named = instances.get(protocol).filter(|i| !i.is_empty());
            let named = named.ok_or(Status::NotFound)?;
            named
                .iter()
                .map(|(name, i)| (name.clone(), i.permission.clone()))
                .collect()
        };
        let mut listed = Vec::new();
        for (name, permission) in named {
            if self.allowed(caller, permission.as_deref()).await {
                listed.push(name);
            }
        }
        Ok(listed)
    }
}

fn clone(identity: &ClientIdentity) -> ClientIdentity {
    ClientIdentity {
        pid: identity.pid,
        user: identity.user,
        app_id: identity.app_id.clone(),
    }
}

async fn serve(dir: Rc<Dir>, end: ServerEnd<Directory>, caller: Rc<ClientIdentity>) {
    use DirectoryRequest as R;
    dir.wait_ready().await;
    let server = Rc::new(DirectoryAsyncServer::new(end, dir.reactor.clone()));
    while let Ok(request) = server.next().await {
        // Each request may wait for the broker, so each runs on its own.
        let (dir, server, caller) = (dir.clone(), server.clone(), caller.clone());
        dir.spawner.clone().spawn(async move {
            let s = server.server();
            let _ = match request {
                R::Connect(q, r) => {
                    let result = dir.connect(&caller, q.protocol, q.instance, q.server).await;
                    r.reply(s, result.map(|()| DirectoryConnectResponse {}))
                }
                R::List(q, r) => {
                    let result = dir.list(&caller, &q.protocol).await;
                    r.reply(
                        s,
                        result.map(|instances| DirectoryListResponse { instances }),
                    )
                }
                R::Watch(q, r) => {
                    let result = if dir.provided.borrow().contains_key(&q.protocol) {
                        Err(Status::InvalidArgs)
                    } else {
                        dir.watchers.borrow_mut().push(Watcher {
                            protocol: q.protocol,
                            caller: caller.clone(),
                            server: Rc::downgrade(&server),
                        });
                        Ok(DirectoryWatchResponse {})
                    };
                    r.reply(s, result)
                }
            };
        });
    }
}
