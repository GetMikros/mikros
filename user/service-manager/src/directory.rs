//! The service directory: `Directory` on every process's bound
//! connection, forwarding connections to the services that provide
//! each protocol.
//!
//! A protocol that a manifest provides has one provider and no
//! instance names; connections to it wait while its service is not
//! running yet, and fail once it will not run. Named instances are kept
//! apart: the registrar adds them, and they go when they are
//! unregistered, when their provider closes, or when the bound
//! connection of the process that registered them closes. The service
//! manager provides the registrar itself, as `mikros.service.Registrar`,
//! and `mikros.service.Control`, whose `Shutdown` it hands to the
//! supervisor.
//! When a protocol or instance has a permission, the caller must hold
//! it, as the broker decides; without an answer from the broker, it
//! does not.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::{Future, poll_fn};
use core::pin::Pin;
use core::task::{Poll, Waker};
use mikros_manifest::Service;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, DirectoryAsyncServer, DirectoryConnectResponse,
    DirectoryListResponse, DirectoryRequest, DirectoryWatchResponse, Provider as ProviderProtocol,
    ProviderClient, ShutdownKind,
};

/// The protocol of the registrar, which the service manager provides,
/// and the permission it needs.
pub const REGISTRAR: &str = "mikros.service.Registrar";
const REGISTER_PERMISSION: &str = "services.register";
/// The protocol through which the machine is shut down, which the
/// service manager provides, and the permission it needs.
pub const CONTROL: &str = "mikros.service.Control";
const CONTROL_PERMISSION: &str = "power.control";
const PEER_CLOSED: u32 = 1 << 2;

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
    /// The service manager's own tasks that wait for a protocol to get a
    /// named instance.
    awaiting: RefCell<Vec<Waker>>,
    /// The number of instances registered so far, which tells each
    /// one apart from a later one of the same name.
    registered: Cell<u64>,
    demand: RefCell<Option<Demand>>,
    shutdown: RefCell<Option<Shutdown>>,
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
    /// An on-demand service that is not running; the first connection
    /// starts it.
    Idle,
    /// The service manager's own registrar.
    Registrar,
    /// The service manager's own `Control`.
    Control,
}

/// What the protocols of a service start as.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Initially {
    /// The service starts at boot; connections wait for it.
    Starting,
    /// An on-demand service, started by the first connection.
    Idle,
    /// The service will not run.
    Never,
}

/// What starts an idle service, by name.
type Demand = Rc<dyn Fn(&str)>;

/// What shuts the machine down for `Shutdown`.
pub type Shutdown = Rc<dyn Fn(ShutdownKind) -> Pin<Box<dyn Future<Output = Result<(), Status>>>>>;

/// A named instance of a protocol.
struct Instance {
    permission: Option<String>,
    provider: ProviderClient,
    /// The pid of the process that registered it.
    owner: u32,
    /// Which registration this is.
    number: u64,
}

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
            awaiting: RefCell::new(Vec::new()),
            registered: Cell::new(0),
            demand: RefCell::new(None),
            shutdown: RefCell::new(None),
        })
    }

    /// Serves `Directory` on `end` for `caller` until it closes.
    pub fn serve(self: &Rc<Self>, end: ServerEnd<Directory>, caller: ClientIdentity) {
        let dir = self.clone();
        self.spawner.spawn(serve(dir, end, Rc::new(caller)));
    }

    /// Records the protocols that `services` provide, as `initially`
    /// says for each service, and lets requests through. A protocol that
    /// two services provide is left unprovided.
    pub fn set_services(&self, services: &[Service], initially: impl Fn(usize) -> Initially) {
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
            let provider = match initially(i) {
                Initially::Starting => Provider::Waiting(Vec::new()),
                Initially::Idle => Provider::Idle,
                Initially::Never => Provider::Never,
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
        for own in [REGISTRAR, CONTROL] {
            if provided.contains_key(own) {
                mikros_rt::log!(
                    Error,
                    "protocol {own} is the service manager's own, not a service's"
                );
            }
        }
        let registrar = Provided {
            service: String::new(),
            permission: Some(REGISTER_PERMISSION.into()),
            provider: Provider::Registrar,
        };
        provided.insert(REGISTRAR.into(), registrar);
        let control = Provided {
            service: String::new(),
            permission: Some(CONTROL_PERMISSION.into()),
            provider: Provider::Control,
        };
        provided.insert(CONTROL.into(), control);
        drop(provided);
        self.ready.set(true);
        for waker in self.waiting.take() {
            waker.wake();
        }
    }

    pub fn spawner(&self) -> &Spawner {
        &self.spawner
    }

    pub fn reactor(&self) -> &Reactor {
        &self.reactor
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

    /// Calls `shutdown` for `Shutdown` on `Control`.
    pub fn set_shutdown(&self, shutdown: Shutdown) {
        *self.shutdown.borrow_mut() = Some(shutdown);
    }

    /// What shuts the machine down, once the supervisor set it.
    pub fn shutdown(&self) -> Option<Shutdown> {
        self.shutdown.borrow().clone()
    }

    /// Calls `demand` with the name of an idle service when a connection
    /// to it arrives.
    pub fn set_demand(&self, demand: Demand) {
        *self.demand.borrow_mut() = Some(demand);
    }

    /// Makes `service`, an on-demand service that ended, idle again, so
    /// that the next connection starts it.
    pub fn idle(&self, service: &str) {
        for p in self.provided.borrow_mut().values_mut() {
            if p.service == service {
                p.provider = Provider::Idle;
            }
        }
    }

    /// Keeps the connections to the protocols of `service`, which ended
    /// and is restarting, until it runs again.
    pub fn stopped(&self, service: &str) {
        for p in self.provided.borrow_mut().values_mut() {
            if p.service == service {
                p.provider = Provider::Waiting(Vec::new());
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
        let broker = self.broker.borrow().clone();
        allowed(broker, caller, permission).await
    }

    pub async fn connect(
        self: &Rc<Self>,
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
        let mut provided = self.provided.borrow_mut();
        if let Some(p) = provided.get_mut(&protocol) {
            let client = clone(caller);
            let demanded = match &mut p.provider {
                Provider::Waiting(waiting) => {
                    waiting.push((server, client));
                    None
                }
                Provider::Running(provider) => {
                    let _ = provider.connect(protocol, None, server, client);
                    None
                }
                Provider::Never => return Err(Status::NotFound),
                Provider::Idle => {
                    p.provider = Provider::Waiting(alloc::vec![(server, client)]);
                    Some(p.service.clone())
                }
                Provider::Registrar => {
                    drop(provided);
                    let server = ServerEnd::new(server);
                    crate::registrar::serve(self.clone(), server, clone(caller));
                    return Ok(());
                }
                Provider::Control => {
                    drop(provided);
                    crate::control::serve(self.clone(), ServerEnd::new(server));
                    return Ok(());
                }
            };
            if let Some(service) = demanded {
                // Every protocol of the service waits for it now.
                for q in provided.values_mut() {
                    if q.service == service && matches!(q.provider, Provider::Idle) {
                        q.provider = Provider::Waiting(Vec::new());
                    }
                }
                drop(provided);
                let demand = self.demand.borrow().clone();
                if let Some(demand) = demand {
                    demand(&service);
                }
            }
            return Ok(());
        }
        drop(provided);
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
    pub async fn list(
        &self,
        caller: &ClientIdentity,
        protocol: &str,
    ) -> Result<Vec<String>, Status> {
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

impl Dir {
    /// Completes once `protocol` has a named instance.
    pub async fn published(&self, protocol: &str) {
        poll_fn(|cx| {
            let instances = self.instances.borrow();
            if instances.get(protocol).is_some_and(|i| !i.is_empty()) {
                return Poll::Ready(());
            }
            self.awaiting.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// Adds `instance` of `protocol` for `owner`, forwarded to
    /// `provider` and protected by `permission`, and tells the watchers.
    pub async fn register(
        self: &Rc<Self>,
        owner: &ClientIdentity,
        protocol: String,
        instance: String,
        permission: Option<String>,
        provider: ClientEnd<ProviderProtocol>,
    ) -> Result<(), Status> {
        self.check_free(&protocol, &instance)?;
        if let Some(permission) = &permission {
            self.known(owner, permission).await?;
            // The broker may have taken a while.
            self.check_free(&protocol, &instance)?;
        }
        let number = self.registered.get() + 1;
        self.registered.set(number);
        let channel = provider.handle().raw();
        let added = Instance {
            permission: permission.clone(),
            provider: ProviderClient::new(provider),
            owner: owner.pid,
            number,
        };
        self.instances
            .borrow_mut()
            .entry(protocol.clone())
            .or_default()
            .insert(instance.clone(), added);
        self.tell(&protocol, &instance, permission, true);
        for waker in self.awaiting.take() {
            waker.wake();
        }
        // The instance goes when its provider closes. The channel stays
        // open while the instance is kept, and a later instance of the
        // same name has another number.
        let dir = self.clone();
        self.spawner.spawn(async move {
            if dir.reactor.signals(channel, PEER_CLOSED).await.is_ok() {
                dir.remove(&protocol, &instance, |i| i.number == number);
            }
        });
        Ok(())
    }

    /// Removes `owner`'s `instance` of `protocol`.
    pub fn unregister(
        &self,
        owner: &ClientIdentity,
        protocol: &str,
        instance: &str,
    ) -> Result<(), Status> {
        let instances = self.instances.borrow();
        let found = instances
            .get(protocol)
            .and_then(|named| named.get(instance));
        let found = found.ok_or(Status::NotFound)?;
        if found.owner != owner.pid {
            return Err(Status::AccessDenied);
        }
        drop(instances);
        self.remove(protocol, instance, |_| true);
        Ok(())
    }

    /// Fails with `ALREADY_EXISTS` when `instance` of `protocol` exists
    /// or the protocol is provided otherwise.
    fn check_free(&self, protocol: &str, instance: &str) -> Result<(), Status> {
        let taken = self.provided.borrow().contains_key(protocol)
            || self
                .instances
                .borrow()
                .get(protocol)
                .is_some_and(|named| named.contains_key(instance));
        if taken {
            Err(Status::AlreadyExists)
        } else {
            Ok(())
        }
    }

    /// Fails with `INVALID_ARGS` when the broker does not know
    /// `permission`, and with `ACCESS_DENIED` when it cannot be asked.
    async fn known(&self, owner: &ClientIdentity, permission: &str) -> Result<(), Status> {
        let broker = self.broker.borrow().clone();
        let broker = broker.ok_or(Status::AccessDenied)?;
        match broker.check(clone(owner), permission.into()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(Status::NotFound)) => Err(Status::InvalidArgs),
            _ => Err(Status::AccessDenied),
        }
    }

    /// Removes `instance` of `protocol` when `which` accepts it, and
    /// tells the watchers.
    fn remove(&self, protocol: &str, instance: &str, which: impl Fn(&Instance) -> bool) {
        let mut instances = self.instances.borrow_mut();
        let Some(named) = instances.get_mut(protocol) else {
            return;
        };
        if !named.get(instance).is_some_and(which) {
            return;
        }
        let removed = named.remove(instance);
        if named.is_empty() {
            instances.remove(protocol);
        }
        drop(instances);
        if let Some(removed) = removed {
            self.tell(protocol, instance, removed.permission, false);
        }
    }

    /// Removes the instances that process `pid` registered, once its
    /// bound connection closed.
    fn closed(&self, pid: u32) {
        let owned: Vec<(String, String)> = self
            .instances
            .borrow()
            .iter()
            .flat_map(|(protocol, named)| {
                named
                    .iter()
                    .filter(|(_, i)| i.owner == pid)
                    .map(|(name, _)| (protocol.clone(), name.clone()))
            })
            .collect();
        for (protocol, instance) in owned {
            self.remove(&protocol, &instance, |i| i.owner == pid);
        }
    }

    /// Sends `OnInstanceAdded`, or `OnInstanceRemoved`, to every live
    /// connection that watches `protocol` and holds `permission`, and
    /// forgets the closed ones.
    fn tell(&self, protocol: &str, instance: &str, permission: Option<String>, added: bool) {
        let mut tell = Vec::new();
        self.watchers.borrow_mut().retain(|w| {
            let Some(server) = w.server.upgrade() else {
                return false;
            };
            if w.protocol == protocol {
                tell.push((server, w.caller.clone()));
            }
            true
        });
        for (server, caller) in tell {
            let (protocol, instance) = (String::from(protocol), String::from(instance));
            let permission = permission.clone();
            let broker = self.broker.borrow().clone();
            self.spawner.spawn(async move {
                if !allowed(broker, &caller, permission.as_deref()).await {
                    return;
                }
                let s = server.server();
                let _ = if added {
                    s.send_on_instance_added(protocol, instance)
                } else {
                    s.send_on_instance_removed(protocol, instance)
                };
            });
        }
    }
}

/// Whether `caller` holds `permission`, if there is one, as `broker`
/// answers.
async fn allowed(
    broker: Option<Rc<BrokerAsyncClient>>,
    caller: &ClientIdentity,
    permission: Option<&str>,
) -> bool {
    let Some(permission) = permission else {
        return true;
    };
    let Some(broker) = broker else {
        return false;
    };
    let checked = broker.check(clone(caller), permission.into()).await;
    matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
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
    // The bound connection closes when its process ends.
    dir.closed(caller.pid);
}
