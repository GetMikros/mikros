//! The root file system, once the process manager says it is available:
//! the service manager's `/usr` and `/etc` are then the root's, it
//! registers the permissions of the program manifests in
//! `/usr/lib/programs/`, gives the device manager the root's `/usr` for
//! its drivers, and starts the services whose manifests in
//! `/usr/lib/services/` it had not read before.

use crate::broker;
use crate::directory::Initially;
use crate::services::{self, TMPFS};
use crate::supervisor::Supervisor;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_manifest::{Start, order};
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::driver::{RootDrivers, RootDriversAsyncClient};
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeAsyncClient, NodeType, Rights};

/// The program manifests' directory, below the root's `/usr`.
const PROGRAMS: &str = "lib/programs";
const PROGRAMS_PATH: &str = "/usr/lib/programs";
const SERVICES: &str = "lib/services";
const SUFFIX: &str = ".yaml";
/// The device manager's protocol for the root's drivers.
const ROOT_DRIVERS: &str = "mikros.driver.RootDrivers";

/// Waits for the root file system, and then acts on its manifests.
pub async fn follow(supervisor: Rc<Supervisor>) {
    let (usr, etc) = supervisor.dir.root().await;
    let reactor = supervisor.reactor.clone();
    let usr = NodeAsyncClient::new(usr, reactor.clone());
    let etc = NodeAsyncClient::new(etc, reactor);
    mikros_rt::log!(Info, "the root file system is available");
    // Read before any service whose manifest the root holds starts.
    let new = new_services(&supervisor, &usr).await;
    let names: Vec<_> = supervisor
        .services
        .borrow()
        .iter()
        .map(|s| s.name.clone())
        .chain(new.iter().map(|s| s.0.name.clone()))
        .collect();
    register_programs(&supervisor, &usr, &names).await;
    give_drivers(&supervisor, &usr).await;
    start_services(&supervisor, new).await;
    *supervisor.root.borrow_mut() = Some((usr, etc));
}

/// Gives the device manager a new read-only connection to the root's
/// `usr`, for the drivers there, under the service manager's own
/// identity. A failure is logged.
async fn give_drivers(supervisor: &Supervisor, usr: &NodeAsyncClient) {
    let given = async {
        let (node, server) = endpoints::<Node>().map_err(|e| alloc::format!("{e:?}"))?;
        match usr.clone(server, Rights::READ).await {
            Ok(Ok(_)) => {}
            failed => return Err(alloc::format!("opening /usr: {failed:?}")),
        }
        let (client, server) = endpoints::<RootDrivers>().map_err(|e| alloc::format!("{e:?}"))?;
        let me = &supervisor.me;
        let connected = supervisor
            .dir
            .connect(me, ROOT_DRIVERS.into(), None, server.into_handle())
            .await;
        connected.map_err(|status| alloc::format!("connecting: {status:?}"))?;
        let client = RootDriversAsyncClient::new(client, supervisor.reactor.clone());
        match client.root_available(node).await {
            Ok(Ok(_)) => Ok(()),
            failed => Err(alloc::format!("{failed:?}")),
        }
    };
    if let Err(why) = given.await {
        mikros_rt::log!(
            Warning,
            "giving the device manager the root's drivers failed: {why}"
        );
    }
}

/// The services of the root's manifests that were not read before.
async fn new_services(
    supervisor: &Supervisor,
    usr: &NodeAsyncClient,
) -> Vec<(mikros_manifest::Service, Vec<u8>)> {
    let read = services::read_all(usr, SERVICES, &supervisor.reactor).await;
    read.into_iter()
        .filter(|(s, _)| supervisor.position(&s.name).is_none())
        .collect()
}

/// Registers the permissions of each valid program manifest with the
/// broker. A manifest that is not valid, or whose app ID is that of a
/// service among `services`, is logged and ignored.
async fn register_programs(
    supervisor: &Supervisor,
    usr: &NodeAsyncClient,
    services: &[alloc::string::String],
) {
    let reactor = &supervisor.reactor;
    let names = match files::list(usr, reactor, PROGRAMS, NodeType::File, SUFFIX).await {
        Ok(names) => names,
        Err(Failure::Fs(Error::NotFound)) => Vec::new(),
        Err(failure) => {
            mikros_rt::log!(Error, "listing {PROGRAMS_PATH} failed: {failure:?}");
            return;
        }
    };
    let Some(broker) = supervisor.broker.borrow().clone() else {
        mikros_rt::log!(
            Error,
            "the permission broker is not running; program manifests are not registered"
        );
        return;
    };
    let mut registered = 0;
    for name in names {
        let path = format!("{PROGRAMS_PATH}/{name}");
        let relative = files::join(PROGRAMS, &name);
        let bytes = match files::read(usr, reactor, &relative, mikros_yaml::SIZE_MAX).await {
            Ok(bytes) => bytes,
            Err(failure) => {
                mikros_rt::log!(Error, "reading {path} failed: {failure:?}");
                continue;
            }
        };
        let program = match mikros_manifest::parse_program(&name, &bytes) {
            Ok(manifest) => manifest.program,
            Err(e) => {
                mikros_rt::log!(
                    Error,
                    "{path}:{}:{}: {:?}; ignored",
                    e.line,
                    e.column,
                    e.kind
                );
                continue;
            }
        };
        let service = services
            .iter()
            .any(|s| program.app_id == format!("system.{s}"));
        if service {
            mikros_rt::log!(
                Error,
                "{path}: {} is a service's app ID; ignored",
                program.app_id
            );
            continue;
        }
        match broker::grant(&broker, &program.app_id, program.permissions).await {
            Ok(()) => registered += 1,
            Err(why) => mikros_rt::log!(Error, "{path}: {why}"),
        }
    }
    mikros_rt::log!(Info, "registered {registered} program manifests");
}

/// Adds the services of `new` and starts the boot services among them,
/// each after the services it waits for, as at boot.
async fn start_services(
    supervisor: &Rc<Supervisor>,
    new: Vec<(mikros_manifest::Service, Vec<u8>)>,
) {
    if new.is_empty() {
        return;
    }
    let first = supervisor.services.borrow().len();
    let (added, manifests): (Vec<_>, Vec<_>) = new.into_iter().unzip();
    supervisor
        .services
        .borrow_mut()
        .extend(added.iter().cloned());
    supervisor.manifests.borrow_mut().extend(manifests);
    let all = supervisor.services.borrow().clone();
    let plan = order(&all, broker::NAME, &[TMPFS]);
    for (i, why) in plan.blocked.iter().filter(|(i, _)| *i >= first) {
        mikros_rt::log!(Error, "service {} will not start: {why:?}", all[*i].name);
    }
    supervisor.dir.add_services(first, &added, |i| {
        if plan.blocked.iter().any(|(b, _)| *b == i) {
            Initially::Never
        } else if all[i].start == Start::Boot {
            Initially::Starting
        } else {
            Initially::Idle
        }
    });
    mikros_rt::log!(
        Info,
        "read {} more service manifests from the root file system",
        added.len()
    );
    for &i in plan.start.iter().filter(|&&i| i >= first) {
        if !supervisor.active.borrow().contains(&i) && !supervisor.stopping.get() {
            supervisor.start_with_after(i, Vec::new()).await;
        }
    }
}
