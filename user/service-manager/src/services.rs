//! The services of the manifests in `usr/lib/services/`.

use crate::BootHandles;
use crate::files::{self, Failure};
use alloc::format;
use alloc::vec::Vec;
use mikros_manifest::{Service, order};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient};

/// The directory of the service manifests, relative to the root.
const SERVICES: &str = "usr/lib/services";
const SUFFIX: &str = ".yaml";
/// The permission broker, which starts before every other service.
const BROKER: &str = "permissions";

/// Reads the service manifests under `root`, and works out the order in
/// which the boot services start.
pub async fn start(root: NodeAsyncClient, reactor: Reactor, _boot: BootHandles) {
    let services = read_all(&root, &reactor).await;
    let order = order(&services, BROKER);
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
}

/// The services of the valid manifests. Files that cannot be read or
/// are invalid are logged and left out, and so are the keys that a
/// manifest does not have.
async fn read_all(root: &NodeAsyncClient, reactor: &Reactor) -> Vec<Service> {
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
                services.push(manifest.service);
            }
            Err(e) => {
                mikros_rt::log!(Error, "/{path}:{}:{}: {:?}", e.line, e.column, e.kind);
            }
        }
    }
    services
}
