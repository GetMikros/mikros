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
//! `mikros.service.Control`, whose `Shutdown` it hands to the
//! supervisor, and `mikros.service.ServiceControl`, whose calls it hands
//! to the supervisor too.
//! When a protocol or instance has a permission, the caller must hold
//! it, as the broker decides; without an answer from the broker, it
//! does not. To connect, an `admin` permission is asked for with
//! `Request`, so that an administrator may allow it; every other
//! permission, and every list and event, is checked with `Check`.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
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
use mikros_rt::interfaces::mikros::fs::Node;
use mikros_rt::interfaces::mikros::packages::PackageInfo;
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, DirectoryAsyncServer, DirectoryConnectResponse,
    DirectoryListResponse, DirectoryRequest, DirectoryWatchResponse, Provider as ProviderProtocol,
    ProviderClient, ServiceEntry, ShutdownKind,
};

/// The protocol of the registrar, which the service manager provides,
/// and the permission it needs.
pub const REGISTRAR: &str = "mikros.service.Registrar";
const REGISTER_PERMISSION: &str = "system.services.create";
/// The protocol through which the machine is shut down, which the
/// service manager provides, and the permission it needs.
pub const CONTROL: &str = "mikros.service.Control";
/// The service manager's list of services, for every process.
const SERVICES: &str = "mikros.service.Services";
const CONTROL_PERMISSION: &str = "admin.power.control";
/// The user permission that `Control` takes as well (MIKSPEC-0036.R45).
const USER_CONTROL_PERMISSION: &str = "user.power.control";
/// The protocol that starts and stops services, and the permission it
/// needs.
const SERVICE_CONTROL: &str = "mikros.service.ServiceControl";
pub const SERVICE_CONTROL_PERMISSION: &str = "admin.services.control";
/// The prefix of the app IDs of the system's programs and services.
const SYSTEM_PREFIX: &str = "system.";
/// The kind of a driver package in `PackageInfo`.
const DRIVER_KIND: &str = "driver";
const PACKAGE_SERVICES: &str = "mikros.service.PackageServices";
/// The only caller of `PackageServices`.
const INSTALL_SERVICE: &str = "system.pkgsvc";
const PEER_CLOSED: u32 = 1 << 2;
/// The start of the names of `admin` permissions.
const ADMIN_SCOPE: &str = "admin.";

/// The root file system's `/system`, `/system/config`, `/system/drivers`
/// and the service manager's `/system/state/servicemgr`.
pub type Root = (
    ClientEnd<Node>,
    ClientEnd<Node>,
    ClientEnd<Node>,
    ClientEnd<Node>,
);

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
    listing: RefCell<Option<Listing>>,
    manage: RefCell<Option<Manage>>,
    packages: RefCell<Option<Packages>>,
    app_of: RefCell<Option<AppOf>>,
    /// The identity each process was bound with last.
    identities: RefCell<BTreeMap<u32, Rc<ClientIdentity>>>,
    /// The names of the disabled services (MIKSPEC-0036.R49).
    disabled: RefCell<BTreeSet<String>>,
    /// The root file system's `/system`, `/system/config` and
    /// `/system/drivers` once the process manager gave them, until the
    /// supervisor takes them, whether it has given them, and the task that
    /// waits for them.
    root: RefCell<Option<Root>>,
    root_given: Cell<bool>,
    root_waiting: RefCell<Option<Waker>>,
}

/// A protocol that a manifest provides.
struct Provided {
    /// The service that provides it.
    service: String,
    permission: Option<String>,
    /// The only app IDs that may connect, or none for any
    /// (MIKSPEC-0101.R66).
    admits: Vec<String>,
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
    /// The service manager's own `Services`.
    Services,
    /// The service manager's own `ServiceControl`.
    ServiceControl,
    /// The service manager's own `PackageServices`, for the install
    /// service only.
    PackageServices,
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
/// What answers `List` on `Services`: the services after a name, and
/// whether more follow.
pub type Listing = Rc<dyn Fn(Option<&str>) -> (Vec<ServiceEntry>, bool)>;
/// What answers a call on `ServiceControl`: the call, for the service
/// of a name.
pub type Manage = Rc<dyn Fn(Call, String) -> Pin<Box<dyn Future<Output = Result<(), Status>>>>>;

/// The packages installed for every user, with their kinds and owners.
pub type Packages = Rc<dyn Fn() -> Pin<Box<dyn Future<Output = Vec<PackageInfo>>>>>;
/// The app ID of the service of a package of a name.
pub type AppOf = Rc<dyn Fn(&str) -> Option<String>>;

/// A call on `ServiceControl`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
    /// `ServicesChanged` of the install service (MIKSPEC-0036.R40).
    ServicesChanged,
}

/// A named instance of a protocol.
struct Instance {
    permission: Option<String>,
    /// The only app IDs that may connect, or none for any: an installed
    /// driver's own family (MIKSPEC-0036.R50).
    admits: Vec<String>,
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
            listing: RefCell::new(None),
            manage: RefCell::new(None),
            packages: RefCell::new(None),
            app_of: RefCell::new(None),
            identities: RefCell::new(BTreeMap::new()),
            root: RefCell::new(None),
            root_given: Cell::new(false),
            root_waiting: RefCell::new(None),
            disabled: RefCell::new(BTreeSet::new()),
        })
    }

    /// Serves `Directory` on `end` for `caller` until it closes.
    /// The identity that `caller`'s process was bound with last: a later
    /// `Bind` for the same process, after it was elevated, replaces the
    /// identity of its earlier connections.
    fn identity(&self, caller: &Rc<ClientIdentity>) -> Rc<ClientIdentity> {
        let latest = self.identities.borrow().get(&caller.pid).cloned();
        latest.unwrap_or_else(|| caller.clone())
    }

    pub fn serve(self: &Rc<Self>, end: ServerEnd<Directory>, caller: ClientIdentity) {
        let dir = self.clone();
        let caller = Rc::new(caller);
        self.identities
            .borrow_mut()
            .insert(caller.pid, caller.clone());
        self.spawner.spawn(serve(dir, end, caller));
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
                    admits: Vec::new(),
                    provider,
                },
            );
        }
        for own in [
            REGISTRAR,
            CONTROL,
            SERVICES,
            SERVICE_CONTROL,
            PACKAGE_SERVICES,
        ] {
            if provided.contains_key(own) {
                mikros_rt::log!(
                    Error,
                    "protocol {own} is the service manager's own, not a service's"
                );
            }
        }
        // Installed apps and drivers register without a permission, for
        // their own protocols (MIKSPEC-0036.R50).
        let registrar = Provided {
            service: String::new(),
            permission: None,
            admits: Vec::new(),
            provider: Provider::Registrar,
        };
        provided.insert(REGISTRAR.into(), registrar);
        let control = Provided {
            service: String::new(),
            permission: Some(CONTROL_PERMISSION.into()),
            admits: Vec::new(),
            provider: Provider::Control,
        };
        provided.insert(CONTROL.into(), control);
        let services = Provided {
            service: String::new(),
            permission: None,
            admits: Vec::new(),
            provider: Provider::Services,
        };
        provided.insert(SERVICES.into(), services);
        let service_control = Provided {
            service: String::new(),
            // Each call needs the permission, but those for a caller's own
            // services (MIKSPEC-0036.R52).
            permission: None,
            admits: Vec::new(),
            provider: Provider::ServiceControl,
        };
        provided.insert(SERVICE_CONTROL.into(), service_control);
        let package_services = Provided {
            service: String::new(),
            permission: None,
            admits: Vec::new(),
            provider: Provider::PackageServices,
        };
        provided.insert(PACKAGE_SERVICES.into(), package_services);
        drop(provided);
        self.ready.set(true);
        for waker in self.waiting.take() {
            waker.wake();
        }
    }

    /// Records the protocols that `services`, added after the first
    /// `first` services, provide, as `initially` says for each by its
    /// index. A protocol that an earlier service or two of them provide
    /// is left as it was.
    pub fn add_services(
        &self,
        first: usize,
        services: &[Service],
        initially: impl Fn(usize) -> Initially,
    ) {
        let mut providers = BTreeMap::<String, Vec<usize>>::new();
        for (k, s) in services.iter().enumerate() {
            for p in &s.provides {
                providers.entry(p.protocol.clone()).or_default().push(k);
            }
        }
        let mut provided = self.provided.borrow_mut();
        for (protocol, by) in providers {
            let ([k], false) = (&by[..], provided.contains_key(&protocol)) else {
                mikros_rt::log!(
                    Error,
                    "protocol {protocol} is provided by more than one service, so the new one does not"
                );
                continue;
            };
            let s = &services[*k];
            let provide = s.provides.iter().find(|p| p.protocol == protocol);
            let permission = provide.and_then(|p| p.permission.clone());
            // A package's service may admit only its own app, or also
            // the apps it lists (MIKSPEC-0101.R66).
            let admits = match provide {
                Some(p) if p.own_app => alloc::vec![s.app_id.clone()],
                Some(p) if !p.allow.is_empty() => core::iter::once(s.app_id.clone())
                    .chain(p.allow.iter().cloned())
                    .collect(),
                _ => Vec::new(),
            };
            let provider = match initially(first + k) {
                Initially::Starting => Provider::Waiting(Vec::new()),
                Initially::Idle => Provider::Idle,
                Initially::Never => Provider::Never,
            };
            provided.insert(
                protocol,
                Provided {
                    service: s.name.clone(),
                    permission,
                    admits,
                    provider,
                },
            );
        }
    }

    /// Whether the service `name` is disabled.
    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.borrow().contains(name)
    }

    /// The names of the disabled services.
    pub fn disabled(&self) -> BTreeSet<String> {
        self.disabled.borrow().clone()
    }

    /// Takes `disabled` as the names of the disabled services.
    pub fn set_disabled(&self, disabled: BTreeSet<String>) {
        *self.disabled.borrow_mut() = disabled;
    }

    /// Stops providing the protocols of the service `name`, whose
    /// package was removed.
    pub fn withdraw(&self, name: &str) {
        self.provided.borrow_mut().retain(|_, p| p.service != name);
    }

    /// Takes the root file system's `/system` and `/system/config` from the
    /// process manager, once: a second time fails with `BAD_STATE`.
    pub fn give_root(
        &self,
        system: ClientEnd<Node>,
        config: ClientEnd<Node>,
        drivers: ClientEnd<Node>,
        state: ClientEnd<Node>,
    ) -> Result<(), Status> {
        if self.root_given.replace(true) {
            return Err(Status::BadState);
        }
        *self.root.borrow_mut() = Some((system, config, drivers, state));
        if let Some(waker) = self.root_waiting.borrow_mut().take() {
            waker.wake();
        }
        Ok(())
    }

    /// The root file system's `/system`, `/system/config` and
    /// `/system/drivers`, once the process manager gave them.
    pub async fn root(&self) -> Root {
        poll_fn(|cx| match self.root.borrow_mut().take() {
            Some(root) => Poll::Ready(root),
            None => {
                *self.root_waiting.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
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

    /// Calls `listing` for `List` on `Services`.
    pub fn set_listing(&self, listing: Listing) {
        *self.listing.borrow_mut() = Some(listing);
    }

    pub fn set_packages(&self, packages: Packages) {
        *self.packages.borrow_mut() = Some(packages);
    }

    pub fn set_app_of(&self, app_of: AppOf) {
        *self.app_of.borrow_mut() = Some(app_of);
    }

    /// The packages installed for every user, once that is set.
    pub async fn packages(&self) -> Vec<PackageInfo> {
        let packages = self.packages.borrow().clone();
        match packages {
            Some(packages) => packages().await,
            None => Vec::new(),
        }
    }

    /// The app ID of the service `name` when it is a package's.
    pub fn app_of(&self, name: &str) -> Option<String> {
        let app_of = self.app_of.borrow().clone();
        app_of.and_then(|f| f(name))
    }

    /// Whether `caller` may start, stop and restart the service `name`
    /// without `admin.services.control`: a service of the caller's own
    /// package, or of a package that the caller's package owns
    /// (MIKSPEC-0036.R52).
    pub async fn owns_service(&self, caller: &ClientIdentity, name: &str) -> bool {
        let Some(app) = self.app_of(name) else {
            return false;
        };
        if app == caller.app_id {
            return true;
        }
        let packages = self.packages().await;
        packages
            .iter()
            .any(|p| p.id == app && !p.owner.is_empty() && p.owner == caller.app_id)
    }

    /// Whether `caller` holds `system.services.create`, with which it
    /// registers any protocol.
    pub async fn may_register(&self, caller: &ClientIdentity) -> bool {
        self.allowed(caller, Some(REGISTER_PERMISSION)).await
    }

    /// Whether the broker allows `caller` the `admin` permission
    /// `permission`, asked for with `reason`.
    pub async fn request(&self, caller: &ClientIdentity, permission: &str, reason: String) -> bool {
        let Some(broker) = self.broker.borrow().clone() else {
            return false;
        };
        let requested = broker
            .request(caller.clone(), permission.into(), reason)
            .await;
        matches!(requested, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }

    /// What answers `List` on `Services`, once it is set.
    pub fn listing(&self) -> Option<Listing> {
        self.listing.borrow().clone()
    }

    /// Calls `manage` for the calls on `ServiceControl`.
    pub fn set_manage(&self, manage: Manage) {
        *self.manage.borrow_mut() = Some(manage);
    }

    /// What answers the calls on `ServiceControl`, once it is set.
    pub fn manage(&self) -> Option<Manage> {
        self.manage.borrow().clone()
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

    /// Whether `caller` may connect to `protocol`, which needs
    /// `permission` if there is one: an `admin` one is requested, with
    /// the connection as the reason.
    async fn may_connect(
        &self,
        caller: &ClientIdentity,
        protocol: &str,
        permission: Option<&str>,
    ) -> bool {
        // Shutting down is for those who hold either power permission, and
        // the user is asked for theirs otherwise (MIKSPEC-0036.R45).
        if protocol == CONTROL {
            if self.allowed(caller, Some(CONTROL_PERMISSION)).await {
                return true;
            }
            let Some(broker) = self.broker.borrow().clone() else {
                return false;
            };
            let reason = alloc::format!("connect to {protocol}");
            let requested = broker
                .request(caller.clone(), USER_CONTROL_PERMISSION.into(), reason)
                .await;
            return matches!(requested, Ok(Ok(r)) if r.decision == Decision::Allowed);
        }
        let Some(permission) = permission.filter(|p| p.starts_with(ADMIN_SCOPE)) else {
            return self.allowed(caller, permission).await;
        };
        let Some(broker) = self.broker.borrow().clone() else {
            return false;
        };
        let reason = alloc::format!("connect to {protocol}");
        let requested = broker
            .request(caller.clone(), permission.into(), reason)
            .await;
        matches!(requested, Ok(Ok(r)) if r.decision == Decision::Allowed)
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
                if !p.admits.is_empty() && !p.admits.contains(&caller.app_id) {
                    return Err(Status::AccessDenied);
                }
                p.permission.clone()
            } else {
                let instances = self.instances.borrow();
                let named = instances.get(&protocol).filter(|i| !i.is_empty());
                let named = named.ok_or(Status::NotFound)?;
                let name = instance.as_ref().ok_or(Status::InvalidArgs)?;
                let found = named.get(name).ok_or(Status::NotFound)?;
                if !found.admits.is_empty() && !found.admits.contains(&caller.app_id) {
                    return Err(Status::AccessDenied);
                }
                found.permission.clone()
            }
        };
        if !self
            .may_connect(caller, &protocol, permission.as_deref())
            .await
        {
            return Err(Status::AccessDenied);
        }
        // The provider may have changed while the broker answered.
        let mut provided = self.provided.borrow_mut();
        if let Some(p) = provided.get_mut(&protocol) {
            let client = caller.clone();
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
                // A disabled service starts only by ServiceControl.
                Provider::Idle if self.disabled.borrow().contains(&p.service) => {
                    return Err(Status::NotFound);
                }
                Provider::Idle => {
                    p.provider = Provider::Waiting(alloc::vec![(server, client)]);
                    Some(p.service.clone())
                }
                Provider::Registrar => {
                    drop(provided);
                    let server = ServerEnd::new(server);
                    crate::registrar::serve(self.clone(), server, caller.clone());
                    return Ok(());
                }
                Provider::Control => {
                    drop(provided);
                    crate::control::serve(self.clone(), ServerEnd::new(server));
                    return Ok(());
                }
                Provider::Services => {
                    drop(provided);
                    crate::status::serve(self.clone(), ServerEnd::new(server));
                    return Ok(());
                }
                Provider::ServiceControl => {
                    drop(provided);
                    crate::manage::serve(self.clone(), ServerEnd::new(server), caller.clone());
                    return Ok(());
                }
                Provider::PackageServices => {
                    drop(provided);
                    if caller.app_id != INSTALL_SERVICE {
                        return Err(Status::AccessDenied);
                    }
                    crate::manage::serve_packages(self.clone(), ServerEnd::new(server));
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
            .connect(protocol, instance, server, caller.clone());
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
    /// An `owner` without `system.services.create`, unless `privileged`,
    /// is an installed app or driver, which registers only protocols
    /// named under its app ID, without a permission; an installed
    /// driver's instances admit only it and the package that owns it
    /// (MIKSPEC-0036.R50).
    pub async fn register(
        self: &Rc<Self>,
        owner: &ClientIdentity,
        protocol: String,
        instance: String,
        permission: Option<String>,
        provider: ClientEnd<ProviderProtocol>,
        privileged: bool,
    ) -> Result<(), Status> {
        let mut admits = Vec::new();
        if !privileged {
            let installed =
                !owner.app_id.starts_with(SYSTEM_PREFIX) && owner.portable_checksum.is_empty();
            let own = alloc::format!("{}.", owner.app_id);
            if !installed || !protocol.starts_with(own.as_str()) || permission.is_some() {
                return Err(Status::AccessDenied);
            }
            let packages = self.packages().await;
            if let Some(driver) = packages
                .iter()
                .find(|p| p.id == owner.app_id && p.kind == DRIVER_KIND)
            {
                admits.push(driver.id.clone());
                if !driver.owner.is_empty() {
                    admits.push(driver.owner.clone());
                }
            }
        }
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
            admits,
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
        match broker.check(owner.clone(), permission.into()).await {
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
    let checked = broker.check(caller.clone(), permission.into()).await;
    matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
}

async fn serve(dir: Rc<Dir>, end: ServerEnd<Directory>, caller: Rc<ClientIdentity>) {
    use DirectoryRequest as R;
    dir.wait_ready().await;
    let server = Rc::new(DirectoryAsyncServer::new(end, dir.reactor.clone()));
    while let Ok(request) = server.next().await {
        // Each request may wait for the broker, so each runs on its own.
        let (dir, server, caller) = (dir.clone(), server.clone(), dir.identity(&caller));
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
    let mut identities = dir.identities.borrow_mut();
    if identities
        .get(&caller.pid)
        .is_some_and(|i| Rc::ptr_eq(i, &caller))
    {
        identities.remove(&caller.pid);
    }
    drop(identities);
    dir.closed(caller.pid);
}
