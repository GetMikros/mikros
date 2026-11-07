//! The services of the manifests in `/usr/lib/services/`.

use crate::boot::BootHandles;
use crate::broker;
use crate::directory::{Dir, Initially};
use crate::supervisor::Supervisor;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_manifest::{Service, order};
use mikros_rt::exec::Reactor;
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient, NodeType};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The directory of the service manifests.
pub const SERVICES: &str = "/usr/lib/services";
/// The service that starts right after the broker.
const TMPFS: &str = "tmpfs";
const SUFFIX: &str = ".yaml";

/// Reads the service manifests in `SERVICES`, which is at the relative
/// path `at` under `fs`, records the protocols they provide in `dir`,
/// and starts the boot services in order as the service manager `me`:
/// the permission broker first, then tmpfs, then each other one once
/// its permissions are registered with the broker. Each gets the boot
/// handles of `boot` that its manifest lists, and is restarted as its
/// manifest says. On-demand services then start at the first connection
/// to one of their protocols.
pub async fn start(
    fs: NodeAsyncClient,
    at: &'static str,
    reactor: Reactor,
    launcher: LauncherAsyncClient,
    me: ClientIdentity,
    dir: Rc<Dir>,
    boot: Rc<BootHandles>,
) {
    let (services, manifests): (Vec<Service>, Vec<Vec<u8>>) =
        read_all(&fs, at, &reactor).await.into_iter().unzip();
    let order = order(&services, broker::NAME, &[TMPFS]);
    for (i, why) in &order.blocked {
        mikros_rt::log!(
            Error,
            "service {} will not start: {why:?}",
            services[*i].name
        );
    }
    mikros_rt::log!(
        Info,
        "read {} service manifests; {} services start at boot",
        services.len(),
        order.start.len()
    );
    dir.set_services(&services, |i| {
        if order.blocked.iter().any(|(b, _)| *b == i) {
            Initially::Never
        } else if order.start.contains(&i) {
            Initially::Starting
        } else {
            Initially::Idle
        }
    });
    let start = order.start.clone();
    let supervisor = Rc::new(Supervisor {
        launcher,
        boot,
        reactor,
        me,
        dir,
        services,
        manifests,
        broker: RefCell::new(None),
        active: RefCell::new(BTreeSet::new()),
        restarts: RefCell::new(BTreeMap::new()),
        demanded: RefCell::new(VecDeque::new()),
        demand_waker: RefCell::new(None),
    });
    let weak = Rc::downgrade(&supervisor);
    supervisor.dir.set_demand(Rc::new(move |name: &str| {
        if let Some(supervisor) = weak.upgrade() {
            supervisor.demand(name);
        }
    }));
    // `order` puts the broker first whenever anything starts; without
    // it, nothing else can.
    let Some((&first, rest)) = start.split_first() else {
        for s in &supervisor.services {
            supervisor.dir.never(&s.name);
        }
        return;
    };
    if !supervisor.start(first).await {
        for s in &supervisor.services {
            supervisor.dir.never(&s.name);
        }
        return;
    }
    for &i in rest {
        supervisor.start(i).await;
    }
    // On-demand services start once the boot services have.
    supervisor.serve_demands().await;
}

/// The services of the valid manifests in `SERVICES`, at `at` under
/// `fs`, with the manifests' bytes. Files that cannot be read or are
/// invalid are logged and left out, and so are the keys that a manifest
/// does not have.
async fn read_all(fs: &NodeAsyncClient, at: &str, reactor: &Reactor) -> Vec<(Service, Vec<u8>)> {
    let names = match files::list(fs, reactor, at, NodeType::File, SUFFIX).await {
        Ok(names) => names,
        Err(Failure::Fs(Error::NotFound)) => Vec::new(),
        Err(failure) => {
            mikros_rt::log!(Error, "listing {SERVICES} failed: {failure:?}");
            Vec::new()
        }
    };
    let mut services = Vec::new();
    for name in names {
        let path = format!("{SERVICES}/{name}");
        let relative = files::join(at, &name);
        let bytes = match files::read(fs, reactor, &relative, mikros_yaml::SIZE_MAX).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                mikros_rt::log!(Error, "reading {path} failed: {failure:?}");
                continue;
            }
        };
        match mikros_manifest::parse(&name, &bytes) {
            Ok(manifest) => {
                for u in &manifest.unknown {
                    mikros_rt::log!(
                        Warning,
                        "{path}:{}:{}: unknown key {}",
                        u.line,
                        u.column,
                        u.key
                    );
                }
                services.push((manifest.service, bytes));
            }
            Err(e) => {
                mikros_rt::log!(Error, "{path}:{}:{}: {:?}", e.line, e.column, e.kind);
            }
        }
    }
    services
}
