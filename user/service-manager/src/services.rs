//! The services of the manifests in `usr/lib/services/`.

use crate::boot::BootHandles;
use crate::broker;
use crate::directory::Dir;
use crate::files::{self, Failure};
use crate::supervisor::Supervisor;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_manifest::{Service, order};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The directory of the service manifests, relative to the root.
const SERVICES: &str = "usr/lib/services";
const SUFFIX: &str = ".yaml";

/// Reads the service manifests under `root`, records the protocols
/// they provide in `dir`, and starts the boot services in order as the
/// service manager `me`: the permission broker first, then each other
/// one once its permissions are registered with the broker. Each gets
/// the boot handles of `boot` that its manifest lists, and is restarted
/// as its manifest says.
pub async fn start(
    root: NodeAsyncClient,
    reactor: Reactor,
    launcher: LauncherAsyncClient,
    me: ClientIdentity,
    dir: Rc<Dir>,
    boot: Rc<BootHandles>,
) {
    let (services, manifests): (Vec<Service>, Vec<Vec<u8>>) =
        read_all(&root, &reactor).await.into_iter().unzip();
    let order = order(&services, broker::NAME);
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
    // TODO: start on-demand services at the first connection; until
    // then, connections to them wait.
    dir.set_services(&services, |i| order.blocked.iter().all(|(b, _)| *b != i));
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
    });
    // `order` puts the broker first whenever anything starts; without
    // it, nothing else can.
    let Some((&first, rest)) = start.split_first() else {
        return;
    };
    if !supervisor.start(first).await {
        for &i in rest {
            supervisor.dir.never(&supervisor.services[i].name);
        }
        return;
    }
    for &i in rest {
        supervisor.start(i).await;
    }
}

/// The services of the valid manifests, with the manifests' bytes.
/// Files that cannot be read or are invalid are logged and left out,
/// and so are the keys that a manifest does not have.
async fn read_all(root: &NodeAsyncClient, reactor: &Reactor) -> Vec<(Service, Vec<u8>)> {
    let names = match files::list(root, reactor, SERVICES, SUFFIX).await {
        Ok(names) => names,
        Err(Failure::Fs(Error::NotFound)) => Vec::new(),
        Err(failure) => {
            mikros_rt::log!(Error, "listing /{SERVICES} failed: {failure:?}");
            Vec::new()
        }
    };
    let mut services = Vec::new();
    for name in names {
        let path = format!("{SERVICES}/{name}");
        let bytes = match files::read(root, reactor, &path, mikros_yaml::SIZE_MAX).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                mikros_rt::log!(Error, "reading /{path} failed: {failure:?}");
                continue;
            }
        };
        match mikros_manifest::parse(&name, &bytes) {
            Ok(manifest) => {
                for u in &manifest.unknown {
                    mikros_rt::log!(
                        Warning,
                        "/{path}:{}:{}: unknown key {}",
                        u.line,
                        u.column,
                        u.key
                    );
                }
                services.push((manifest.service, bytes));
            }
            Err(e) => {
                mikros_rt::log!(Error, "/{path}:{}:{}: {:?}", e.line, e.column, e.kind);
            }
        }
    }
    services
}
