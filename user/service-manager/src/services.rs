//! The services of the manifests in `usr/lib/services/`.

use crate::boot::BootHandles;
use crate::broker;
use crate::directory::Dir;
use crate::files::{self, Failure};
use crate::launch::{self, Started};
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
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
/// the boot handles of `boot` that its manifest lists.
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
    let Some((&first, rest)) = order.start.split_first() else {
        return;
    };
    // `order` puts the broker first whenever anything starts.
    let started = broker::start(
        &launcher,
        &boot,
        &reactor,
        me,
        &services[first],
        &manifests[first],
    );
    let broker = match started.await {
        Ok((started, broker)) => {
            mikros_rt::log!(Info, "started the permission broker as pid {}", started.pid);
            dir.set_broker(broker.clone());
            running(&dir, &reactor, &services[first], started);
            broker
        }
        Err(why) => {
            mikros_rt::log!(Error, "the permission broker did not start: {why}");
            for &i in &order.start {
                dir.never(&services[i].name);
            }
            return;
        }
    };
    for &i in rest {
        let service = &services[i];
        let app_id = format!("system.{}", service.name);
        let started = match broker::grant(&broker, &app_id, service.permissions.clone()).await {
            Ok(()) => launch::launch(&launcher, &boot, service, Vec::new()).await,
            Err(why) => Err(why),
        };
        match started {
            Ok(started) => {
                mikros_rt::log!(
                    Info,
                    "started service {} as pid {}",
                    service.name,
                    started.pid
                );
                running(&dir, &reactor, service, started);
            }
            Err(why) => {
                mikros_rt::log!(Error, "service {} did not start: {why}", service.name);
                dir.never(&service.name);
            }
        }
    }
}

/// Forwards connections to `service`, which `started` runs, and logs
/// when it ends, after which its protocols are no longer provided.
fn running(dir: &Rc<Dir>, reactor: &Reactor, service: &Service, started: Started) {
    dir.running(&service.name, started.provider);
    let (dir, reactor, name) = (dir.clone(), reactor.clone(), service.name.clone());
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        // TODO: restart services as their manifests say.
        match launch::exited(&reactor, &started.process).await {
            Some((code, 0)) => mikros_rt::log!(Info, "service {name} exited with code {code}"),
            Some((_, signal)) => mikros_rt::log!(Info, "service {name} ended by signal {signal}"),
            None => mikros_rt::log!(Error, "service {name} could not be watched"),
        }
        dir.never(&name);
    });
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
