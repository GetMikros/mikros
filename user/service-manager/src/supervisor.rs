//! Starting the boot services, and restarting them as their manifests
//! say.

use crate::boot::BootHandles;
use crate::broker;
use crate::directory::Dir;
use crate::launch::{self, Started};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::future::poll_fn;
use core::pin::Pin;
use core::task::{Poll, Waker};
use mikros_manifest::{Restart, Service, Start};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::permissions::BrokerAsyncClient;
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::sys;

/// A service is restarted at most this many times within `WINDOW`.
const RESTARTS_MAX: usize = 5;
const WINDOW: i64 = 60_000_000_000;

/// The boot services and what the service manager needs to start them.
pub struct Supervisor {
    pub launcher: LauncherAsyncClient,
    pub boot: Rc<BootHandles>,
    pub reactor: Reactor,
    /// The service manager's own identity, for its broker connection.
    pub me: ClientIdentity,
    pub dir: Rc<Dir>,
    pub services: Vec<Service>,
    pub manifests: Vec<Vec<u8>>,
    pub broker: RefCell<Option<Rc<BrokerAsyncClient>>>,
    /// The services that started and have not stopped for good.
    pub active: RefCell<BTreeSet<usize>>,
    /// When each service was restarted, within the last `WINDOW`.
    pub restarts: RefCell<BTreeMap<usize, VecDeque<i64>>>,
    /// The on-demand services that connections asked for, in order.
    pub demanded: RefCell<VecDeque<usize>>,
    pub demand_waker: RefCell<Option<Waker>>,
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
    /// Starts service `i`, and returns whether it started. A service
    /// that did not start is logged and its protocols are no longer
    /// provided.
    pub fn start(self: &Rc<Self>, i: usize) -> Pin<Box<dyn Future<Output = bool>>> {
        let this = self.clone();
        Box::pin(async move {
            let name = this.services[i].name.clone();
            match this.launch(i).await {
                Ok(started) => {
                    mikros_rt::log!(Info, "started service {name} as pid {}", started.pid);
                    this.active.borrow_mut().insert(i);
                    this.watch(i, started);
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
        let service = &self.services[i];
        if service.name == broker::NAME {
            let me = ClientIdentity {
                pid: self.me.pid,
                user: self.me.user,
                app_id: self.me.app_id.clone(),
            };
            let (launcher, boot, reactor) = (&self.launcher, &self.boot, &self.reactor);
            let manifest = &self.manifests[i];
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
        let service = &self.services[i];
        let app_id = format!("system.{}", service.name);
        broker::grant(broker, &app_id, service.permissions.clone()).await
    }

    /// Forwards connections to service `i`, which `started` runs, and
    /// when it ends, logs it and restarts it if its manifest says so.
    /// Connections wait while it restarts; otherwise its protocols are
    /// no longer provided.
    fn watch(self: &Rc<Self>, i: usize, started: Started) {
        let name = self.services[i].name.clone();
        let Started {
            process, provider, ..
        } = started;
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
            let after = ended.map_or(After::Stop, |ended| this.after(i, ended));
            match after {
                After::Restart => {
                    this.dir.stopped(&name);
                    this.start(i).await;
                }
                After::Stop if this.services[i].start == Start::OnDemand => {
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
        let service = &self.services[i];
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
        let Some(i) = self.services.iter().position(|s| s.name == name) else {
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
            if !self.active.borrow().contains(&i) {
                self.start_with_after(i, Vec::new()).await;
            }
        }
    }

    /// Starts the services that service `i` lists in `after` and that
    /// are not active, each with its own, then `i`. `chain` holds the
    /// services that wait for `i` to start; meeting one again is a
    /// cycle. Returns whether `i` started.
    fn start_with_after(
        self: &Rc<Self>,
        i: usize,
        chain: Vec<usize>,
    ) -> Pin<Box<dyn Future<Output = bool>>> {
        let this = self.clone();
        Box::pin(async move {
            let name = this.services[i].name.clone();
            let mut chain = chain;
            chain.push(i);
            for after in this.services[i].after.clone() {
                let Some(j) = this.services.iter().position(|s| s.name == after) else {
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
