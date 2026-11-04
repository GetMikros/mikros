//! The services of the manifests in `usr/lib/services/`.

use crate::BootHandles;
use crate::files::{self, Failure};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient};

/// The directory of the service manifests, relative to the root.
const SERVICES: &str = "usr/lib/services";
const SUFFIX: &str = ".yaml";

/// Reads the service manifests under `root`.
pub async fn start(root: NodeAsyncClient, reactor: Reactor, _boot: BootHandles) {
    let manifests = read_all(&root, &reactor).await;
    mikros_rt::log!(Info, "read {} service manifests", manifests.len());
}

/// The parsed manifests, with their paths. Files that cannot be read or
/// are not valid YAML are logged and left out.
async fn read_all(root: &NodeAsyncClient, reactor: &Reactor) -> Vec<(String, mikros_yaml::Node)> {
    let names = match files::list(root, reactor, SERVICES, SUFFIX).await {
        Ok(names) => names,
        Err(Failure::Fs(Error::NotFound)) => Vec::new(),
        Err(failure) => {
            mikros_rt::log!(Error, "listing /{SERVICES} failed: {failure:?}");
            Vec::new()
        }
    };
    let mut manifests = Vec::new();
    for name in names {
        let path = format!("{SERVICES}/{name}");
        let bytes = match files::read(root, reactor, &path, mikros_yaml::SIZE_MAX).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                mikros_rt::log!(Error, "reading /{path} failed: {failure:?}");
                continue;
            }
        };
        match mikros_yaml::parse(&bytes) {
            Ok(node) => manifests.push((path, node)),
            Err(e) => {
                mikros_rt::log!(Error, "/{path}:{}:{}: {:?}", e.line, e.column, e.kind);
            }
        }
    }
    manifests
}
