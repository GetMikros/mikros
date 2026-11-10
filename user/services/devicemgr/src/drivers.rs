//! The installed drivers: the manifests in `usr/lib/drivers/`, one in
//! each driver's own directory.

use alloc::format;
use alloc::vec::Vec;
use mikros_manifest::Driver;
use mikros_rt::exec::Reactor;
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient, NodeType};

/// The directory of the drivers.
pub const DRIVERS: &str = "/usr/lib/drivers";
/// The name of the manifest in each driver's directory.
const MANIFEST: &str = "driver.yaml";

/// The drivers of the valid manifests in `DRIVERS`, which is at the
/// relative path `at` under `dir`, in name order. Manifests that cannot
/// be read or are invalid are logged and left out, and so are the keys
/// that a manifest does not have.
pub async fn read_all(dir: &NodeAsyncClient, at: &str, reactor: &Reactor) -> Vec<Driver> {
    let directories = match files::list(dir, reactor, at, NodeType::Directory, "").await {
        Ok(names) => names,
        Err(Failure::Fs(Error::NotFound)) => Vec::new(),
        Err(failure) => {
            mikros_rt::log!(
                Error,
                "device-manager: listing {DRIVERS} failed: {failure:?}"
            );
            Vec::new()
        }
    };
    let mut drivers = Vec::new();
    for directory in directories {
        let relative = files::join(at, &format!("{directory}/{MANIFEST}"));
        let path = format!("{DRIVERS}/{directory}/{MANIFEST}");
        let bytes = match files::read(dir, reactor, &relative, mikros_yaml::SIZE_MAX).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                mikros_rt::log!(Error, "device-manager: reading {path} failed: {failure:?}");
                continue;
            }
        };
        match mikros_manifest::parse_driver(&directory, &bytes) {
            Ok(manifest) => {
                for u in &manifest.unknown {
                    mikros_rt::log!(
                        Warning,
                        "device-manager: {path}:{}:{}: unknown key {}",
                        u.line,
                        u.column,
                        u.key
                    );
                }
                drivers.push(manifest.driver);
            }
            Err(e) => mikros_rt::log!(
                Error,
                "device-manager: {path}:{}:{}: {:?}",
                e.line,
                e.column,
                e.kind
            ),
        }
    }
    drivers.sort_by(|a, b| a.name.cmp(&b.name));
    drivers
}
