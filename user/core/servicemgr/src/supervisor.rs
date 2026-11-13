//! Starting the boot services, and restarting them as their manifests
//! say; and stopping them all at shutdown, the last started first, before
//! the machine is powered off or restarted through the ACPI server.

use crate::boot::BootHandles;
use crate::broker;
use crate::directory::Dir;
use crate::launch::{self, Started};
use crate::sources::{SOURCES, Sources};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::future::poll_fn;
use core::pin::Pin;
use core::task::{Poll, Waker};
use mikros_manifest::{Restart, Service, Start};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::acpi::{Acpi, AcpiAsyncClient, AcpiEvent};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    Shutdown as DriverShutdown, ShutdownAsyncClient as DriverShutdownAsyncClient,
    ShutdownKind as DriverShutdownKind,
};
use mikros_rt::interfaces::mikros::permissions::BrokerAsyncClient;
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::{ClientIdentity, ShutdownKind};
use mikros_rt::sys;

/// A service is restarted at most this many times within `WINDOW`.
const RESTARTS_MAX: usize = 5;
const WINDOW: i64 = 60_000_000_000;
/// The signals that stop a service, and how often a stop looks whether
/// the service has terminated.
const TERM: u32 = 15;
const KILL: u32 = 9;
const POLL: i64 = 10_000_000;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// The ACPI server's service, which powers the machine off and restarts
/// it.
const ACPI: &str = "device.acpi";
/// The device manager's protocol that is told the kind of shutdown.
const DRIVER_SHUTDOWN: &str = "mikros.driver.Shutdown";
/// The protocol by which the log service is known: it keeps running when
/// the other services stop at shutdown.
const LOG_WRITER: &str = "mikros.log.Writer";

/// The boot services and what the service manager needs to start them.
pub struct Supervisor {
    pub launcher: LauncherAsyncClient,
    pub boot: Rc<BootHandles>,
    pub reactor: Reactor,
    /// The service manager's own identity, for its broker connection.
    pub me: ClientIdentity,
    /// Where the standard output and error of services go.
    pub sources: Rc<Sources>,
    pub dir: Rc<Dir>,
    /// The services of the manifests read so far, by index; later ones
    /// are added after them.
    pub services: RefCell<Vec<Service>>,
    pub manifests: RefCell<Vec<Vec<u8>>>,
    pub broker: RefCell<Option<Rc<BrokerAsyncClient>>>,
    /// The services that started and have not stopped for good.
    pub active: RefCell<BTreeSet<usize>>,
    /// When each service was restarted, within the last `WINDOW`.
    pub restarts: RefCell<BTreeMap<usize, VecDeque<i64>>>,
    /// The on-demand services that connections asked for, in order.
    pub demanded: RefCell<VecDeque<usize>>,
    pub demand_waker: RefCell<Option<Waker>>,
    /// The running services and their process IDs, in the order they
    /// started.
    pub running: RefCell<Vec<(usize, u32)>>,
    /// Set once `Shutdown` began: from then on, no service is started or
    /// restarted.
    pub stopping: Cell<bool>,
    /// The root file system's `/usr` and `/etc`, once it is available.
    pub root: RefCell<
        Option<(
            mikros_rt::interfaces::mikros::fs::NodeAsyncClient,
            mikros_rt::interfaces::mikros::fs::NodeAsyncClient,
        )>,
    >,
}

/// What happens to a service that ended.
enum After {
    Restart,
    /// It is not restarted; an on-demand one may start again on demand.
    Stop,
    /// It failed too often, and does not start again.
    GiveUp,
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

impl Supervisor {
    /// A copy of service `i`'s manifest.
    pub fn service(&self, i: usize) -> Service {
        self.services.borrow()[i].clone()
    }

    /// The index of the service named `name`.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.services.borrow().iter().position(|s| s.name == name)
    }

    /// Starts service `i`, and returns whether it started. A service
    /// that did not start is logged and its protocols are no longer
    /// provided.
    pub fn start(self: &Rc<Self>, i: usize) -> Pin<Box<dyn Future<Output = bool>>> {
        let this = self.clone();
        Box::pin(async move {
            let name = this.service(i).name;
            match this.launch(i).await {
                Ok(mut started) => {
                    mikros_rt::log!(Info, "started service {name} as pid {}", started.pid);
                    this.active.borrow_mut().insert(i);
                    this.running.borrow_mut().push((i, started.pid));
                    let (pid, streams) = (started.pid, started.streams.take());
                    this.watch(i, started);
                    if let Some((output, error)) = streams {
                        let app_id = format!("system.{name}");
                        this.sources.add(pid, app_id, output, error);
                    }
                    true
                }
                Err(why) => {
                    mikros_rt::log!(Error, "service {name} did not start: {why}");
                    this.active.borrow_mut().remove(&i);
                    this.dir.never(&name);
                    false
                }
            }
        })
    }

    /// Starts service `i`: the broker with its manifest, after which the
    /// permissions of every other active service are registered again,
    /// and any other service once its own are registered.
    async fn launch(&self, i: usize) -> Result<Started, String> {
        let service = self.service(i);
        let service = &service;
        if service.name == broker::NAME {
            let me = ClientIdentity {
                pid: self.me.pid,
                user: self.me.user,
                app_id: self.me.app_id.clone(),
            };
            let (launcher, boot, reactor) = (&self.launcher, &self.boot, &self.reactor);
            let manifest = &self.manifests.borrow()[i].clone();
            let (started, broker) =
                broker::start(launcher, boot, reactor, me, service, manifest).await?;
            *self.broker.borrow_mut() = Some(broker.clone());
            self.dir.set_broker(broker.clone());
            let others: Vec<usize> = self
                .active
                .borrow()
                .iter()
                .copied()
                .filter(|&j| j != i)
                .collect();
            for j in others {
                if let Err(why) = self.grant(&broker, j).await {
                    mikros_rt::log!(Error, "{why}");
                }
            }
            return Ok(started);
        }
        let broker = self.broker.borrow().clone();
        let broker = broker.ok_or_else(|| String::from("the permission broker is not running"))?;
        self.grant(&broker, i).await?;
        launch::launch(&self.launcher, &self.boot, service, Vec::new()).await
    }

    /// Registers the permissions of service `i`'s manifest.
    async fn grant(&self, broker: &BrokerAsyncClient, i: usize) -> Result<(), String> {
        let service = self.service(i);
        let app_id = format!("system.{}", service.name);
        broker::grant(broker, &app_id, service.permissions.clone()).await
    }

    /// Forwards connections to service `i`, which `started` runs, and
    /// when it ends, logs it and restarts it if its manifest says so.
    /// Connections wait while it restarts; otherwise its protocols are
    /// no longer provided.
    fn watch(self: &Rc<Self>, i: usize, started: Started) {
        let service = self.service(i);
        let name = service.name.clone();
        let Started {
            process, provider, ..
        } = started;
        if service.provides.iter().any(|p| p.protocol == SOURCES) {
            self.sources.log_runs(&provider);
        }
        self.dir.running(&name, provider);
        let this = self.clone();
        self.dir.spawner().clone().spawn(async move {
            let ended = launch::exited(&this.reactor, &process).await;
            match ended {
                Some((code, 0)) => mikros_rt::log!(Info, "service {name} exited with code {code}"),
                Some((_, signal)) => {
                    mikros_rt::log!(Info, "service {name} ended by signal {signal}")
                }
                None => mikros_rt::log!(Error, "service {name} could not be watched"),
            }
            drop(process);
            this.running.borrow_mut().retain(|&(j, _)| j != i);
            if this.stopping.get() {
                this.active.borrow_mut().remove(&i);
                this.dir.never(&name);
                return;
            }
            let after = ended.map_or(After::Stop, |ended| this.after(i, ended));
            match after {
                After::Restart => {
                    this.dir.stopped(&name);
                    this.start(i).await;
                }
                After::Stop if this.service(i).start == Start::OnDemand => {
                    this.active.borrow_mut().remove(&i);
                    this.dir.idle(&name);
                }
                After::Stop | After::GiveUp => {
                    this.active.borrow_mut().remove(&i);
                    this.dir.never(&name);
                }
            }
        });
    }

    /// What happens to service `i`, which ended with `ended`: it is
    /// restarted with `always`, or with `on-failure` after any end but
    /// exit code 0, unless it was restarted `RESTARTS_MAX` times within
    /// `WINDOW`.
    fn after(&self, i: usize, (code, signal): (i64, u32)) -> After {
        let service = self.service(i);
        let failed = code != 0 || signal != 0;
        let wanted = match service.restart {
            Restart::Always => true,
            Restart::OnFailure => failed,
            Restart::Never => false,
        };
        if !wanted {
            return After::Stop;
        }
        let now = now();
        let mut restarts = self.restarts.borrow_mut();
        let times = restarts.entry(i).or_default();
        while times.front().is_some_and(|&t| now - t >= WINDOW) {
            times.pop_front();
        }
        if times.len() >= RESTARTS_MAX {
            mikros_rt::log!(
                Error,
                "service {} failed after {RESTARTS_MAX} restarts within 60 s; it is not restarted again",
                service.name
            );
            return After::GiveUp;
        }
        times.push_back(now);
        After::Restart
    }

    /// Asks for the on-demand service `name`, which a connection waits
    /// for, to be started.
    pub fn demand(&self, name: &str) {
        let Some(i) = self.position(name) else {
            return;
        };
        self.demanded.borrow_mut().push_back(i);
        if let Some(waker) = self.demand_waker.borrow_mut().take() {
            waker.wake();
        }
    }

    /// Starts the demanded services one at a time, for as long as the
    /// service manager runs.
    pub async fn serve_demands(self: &Rc<Self>) {
        loop {
            let i = poll_fn(|cx| match self.demanded.borrow_mut().pop_front() {
                Some(i) => Poll::Ready(i),
                None => {
                    *self.demand_waker.borrow_mut() = Some(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await;
            if !self.stopping.get() && !self.active.borrow().contains(&i) {
                self.start_with_after(i, Vec::new()).await;
            }
        }
    }

    fn is_running(&self, i: usize) -> bool {
        self.running.borrow().iter().any(|&(j, _)| j == i)
    }

    /// Waits until service `i` has terminated or `deadline` has passed:
    /// whether it terminated.
    async fn terminated(&self, i: usize, deadline: i64) -> bool {
        while self.is_running(i) {
            if now() >= deadline {
                return false;
            }
            let _ = self.reactor.sleep_until(now().saturating_add(POLL)).await;
        }
        true
    }

    /// Stops service `i`, which runs as `pid`: sends it `TERM`, and
    /// `KILL` once its stop timeout has passed, and returns once it has
    /// terminated.
    async fn stop(&self, i: usize, pid: u32) {
        let service = self.service(i);
        let timeout = i64::try_from(service.stop_timeout)
            .unwrap_or(i64::MAX)
            .saturating_mul(NANOS_PER_SECOND);
        let _ = self.launcher.kill(pid, TERM).await;
        if !self.terminated(i, now().saturating_add(timeout)).await {
            mikros_rt::log!(
                Warning,
                "service {} did not end within {} s of TERM; killing it",
                service.name,
                service.stop_timeout
            );
            let _ = self.launcher.kill(pid, KILL).await;
            self.terminated(i, i64::MAX).await;
        }
        mikros_rt::log!(Info, "stopped service {}", service.name);
    }

    /// Stops every running service, the last started first, each once
    /// the one started after it has terminated.
    async fn stop_all(&self) {
        loop {
            // The log service is passed over, so that what is logged until
            // the machine powers off or restarts still reaches the kernel
            // log.
            let last = self
                .running
                .borrow()
                .iter()
                .rev()
                .find(|&&(i, _)| !self.is_log_service(i))
                .copied();
            let Some((i, pid)) = last else {
                break;
            };
            self.stop(i, pid).await;
        }
    }

    /// Whether service `i` is the log service, which provides
    /// `LOG_WRITER`.
    fn is_log_service(&self, i: usize) -> bool {
        self.services.borrow()[i]
            .provides
            .iter()
            .any(|p| p.protocol == LOG_WRITER)
    }

    /// Tells the device manager, under the service manager's own identity,
    /// whether the machine is to power off or restart, for the ACPI
    /// server's preparation. A failure is logged.
    async fn set_kind(&self, power_off: bool) {
        let kind = match power_off {
            true => DriverShutdownKind::PowerOff,
            false => DriverShutdownKind::Restart,
        };
        let set = async {
            let (client, server) = endpoints::<DriverShutdown>().map_err(|_| Status::NoMemory)?;
            self.dir
                .connect(&self.me, DRIVER_SHUTDOWN.into(), None, server.into_handle())
                .await?;
            let client = DriverShutdownAsyncClient::new(client, self.reactor.clone());
            client.set_kind(kind).await.map_err(|_| Status::PeerClosed)
        };
        if let Err(status) = set.await {
            mikros_rt::log!(
                Warning,
                "telling the device manager the kind of shutdown failed: {status:?}"
            );
        }
    }

    /// A connection to the ACPI server, under the service manager's own
    /// identity: None when there is none.
    async fn connect_acpi(&self) -> Option<AcpiAsyncClient> {
        let listed = self.dir.list(&self.me, ACPI).await.ok()?;
        let instance = listed.into_iter().next()?;
        let (client, server) = endpoints::<Acpi>().ok()?;
        let connected = self
            .dir
            .connect(&self.me, ACPI.into(), Some(instance), server.into_handle())
            .await;
        connected.ok()?;
        Some(AcpiAsyncClient::new(client, self.reactor.clone()))
    }

    /// Watches the power button: once the ACPI server publishes
    /// `device.acpi`, connects to it and keeps the connection, and each
    /// time the button is pressed shuts the machine down and powers it
    /// off, as `Shutdown` with `POWER_OFF` does, unless a shutdown is in
    /// progress already. It stops watching when the connection fails or
    /// closes.
    pub async fn watch_power_button(self: Rc<Self>) {
        self.dir.published(ACPI).await;
        let Some(acpi) = self.connect_acpi().await else {
            mikros_rt::log!(
                Warning,
                "connecting to the ACPI server for the power button failed"
            );
            return;
        };
        mikros_rt::log!(Info, "watching the power button");
        while let Ok(event) = acpi.next_event().await {
            if !matches!(event, AcpiEvent::OnPowerButton(_)) || self.stopping.get() {
                continue;
            }
            mikros_rt::log!(Info, "the power button was pressed: shutting down");
            // A shutdown returns only when it failed.
            if let Err(status) = self.shutdown(ShutdownKind::PowerOff).await {
                mikros_rt::log!(
                    Error,
                    "shutting down for the power button failed: {status:?}"
                );
            }
        }
    }

    /// Shuts the machine down: connects to the ACPI server, tells the
    /// device manager the kind of shutdown, stops every service but the
    /// log service, and then powers the machine off or restarts it through
    /// that connection, which returns only when that failed. Fails,
    /// stopping nothing, with `INVALID_ARGS` for an unknown kind, with
    /// `NOT_SUPPORTED` without an ACPI server, and with `BAD_STATE` once a
    /// shutdown began.
    pub async fn shutdown(&self, kind: ShutdownKind) -> Result<(), Status> {
        let restart = match kind {
            ShutdownKind::PowerOff => false,
            ShutdownKind::Restart => true,
            ShutdownKind::Unknown(_) => return Err(Status::InvalidArgs),
        };
        if self.stopping.replace(true) {
            return Err(Status::BadState);
        }
        let Some(acpi) = self.connect_acpi().await else {
            self.stopping.set(false);
            return Err(Status::NotSupported);
        };
        self.set_kind(!restart).await;
        mikros_rt::log!(Info, "shutting down: stopping the services");
        self.stop_all().await;
        let done = match restart {
            false => acpi.power_off().await.map(|r| r.map(drop)),
            true => acpi.restart().await.map(|r| r.map(drop)),
        };
        match done {
            Ok(Ok(())) => Ok(()),
            Ok(Err(status)) => Err(status),
            Err(_) => Err(Status::PeerClosed),
        }
    }

    /// Starts the services that service `i` lists in `after` and that
    /// are not active, each with its own, then `i`. `chain` holds the
    /// services that wait for `i` to start; meeting one again is a
    /// cycle. Returns whether `i` started.
    pub fn start_with_after(
        self: &Rc<Self>,
        i: usize,
        chain: Vec<usize>,
    ) -> Pin<Box<dyn Future<Output = bool>>> {
        let this = self.clone();
        Box::pin(async move {
            let name = this.service(i).name;
            let mut chain = chain;
            chain.push(i);
            for after in this.service(i).after {
                let Some(j) = this.position(&after) else {
                    continue;
                };
                let failed = if chain.contains(&j) {
                    Some("waits for itself through after")
                } else if this.active.borrow().contains(&j) {
                    None
                } else if !this.start_with_after(j, chain.clone()).await {
                    Some("a service it waits for did not start")
                } else {
                    None
                };
                if let Some(why) = failed {
                    mikros_rt::log!(Error, "service {name} did not start: {why}");
                    this.dir.never(&name);
                    return false;
                }
            }
            this.start(i).await
        })
    }
}
