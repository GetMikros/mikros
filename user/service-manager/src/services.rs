//! The services of the manifests in `usr/lib/services/`.

use crate::files::{self, Failure};
use crate::{BootHandles, broker};
use alloc::format;
use alloc::vec::Vec;
use mikros_manifest::{Service, order};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient};
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The directory of the service manifests, relative to the root.
const SERVICES: &str = "usr/lib/services";
const SUFFIX: &str = ".yaml";

/// Reads the service manifests under `root`, works out the order in
/// which the boot services start, and starts the permission broker,
/// which comes first, as the service manager `me`.
pub async fn start(
    root: NodeAsyncClient,
    reactor: Reactor,
    _boot: BootHandles,
    launcher: LauncherAsyncClient,
    me: ClientIdentity,
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
    let Some(&first) = order.start.first() else {
        return;
    };
    // `order` puts the broker first whenever anything starts.
    let running = broker::start(&launcher, &reactor, me, &services[first], &manifests[first]);
    let running = match running.await {
        Ok(running) => running,
        Err(why) => {
            mikros_rt::log!(Error, "the permission broker did not start: {why}");
            return;
        }
    };
    mikros_rt::log!(
        Info,
        "started the permission broker as pid {}",
        running.0.pid
    );
    // TODO: register each service's permissions and start it, once
    // services can be given their boot handles.
    for &i in &order.start[1..] {
        mikros_rt::log!(Warning, "service {} is not started yet", services[i].name);
    }
    // The broker's connections stay open for as long as this runs.
    let _running = running;
    core::future::pending::<()>().await;
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
