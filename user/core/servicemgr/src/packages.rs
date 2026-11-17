//! The services of packages (MIKSPEC-0101.R55): those of the globally
//! installed packages, which the install service lists with `Services`
//! and `ServiceManifest`, read once the root file system is available and
//! again whenever it calls `ServicesChanged` (MIKSPEC-0036.R40). New ones
//! start as their manifests say; removed ones stop and no longer provide
//! their protocols, and start again as new ones if they come back.

use crate::directory::Initially;
use crate::supervisor::Supervisor;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::{Service, Start};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::packages::{Installer, InstallerAsyncClient};

const INSTALLER: &str = "mikros.packages.Installer";

/// The services of global packages and their manifests, as the install
/// service lists them; none without an install service.
async fn read(supervisor: &Supervisor) -> Result<Vec<(Service, Vec<u8>)>, Status> {
    let (client, server) = endpoints::<Installer>().map_err(|_| Status::NoResources)?;
    match supervisor
        .dir
        .connect(&supervisor.me, INSTALLER.into(), None, server.into_handle())
        .await
    {
        Ok(()) => {}
        Err(Status::NotFound) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let installer = InstallerAsyncClient::new(client, supervisor.reactor.clone());
    let mut found = Vec::new();
    let mut after = String::new();
    loop {
        let page = match installer.services(after.clone()).await {
            Ok(Ok(r)) => r.services,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Status::PeerClosed),
        };
        let Some(last) = page.last().map(|s| s.name.clone()) else {
            return Ok(found);
        };
        for listed in page {
            let manifest = match installer.service_manifest(listed.name.clone()).await {
                Ok(Ok(r)) => r.manifest,
                failed => {
                    mikros_rt::log!(Error, "service {}: no manifest: {failed:?}", listed.name);
                    continue;
                }
            };
            let prefix = format!("{}.", listed.app_id);
            let Some(key) = listed.name.strip_prefix(prefix.as_str()) else {
                continue;
            };
            match mikros_manifest::parse_package_service(&listed.app_id, key, &manifest) {
                Ok(m) => found.push((m.service, manifest)),
                Err(e) => mikros_rt::log!(
                    Error,
                    "service {}: {}:{}: {:?}",
                    listed.name,
                    e.line,
                    e.column,
                    e.kind
                ),
            }
        }
        after = last;
    }
}

/// Reads the services of packages again, stops those that are gone and
/// starts those that are new.
pub async fn changed(supervisor: &Rc<Supervisor>) -> Result<(), Status> {
    let listed = read(supervisor).await?;
    let current: Vec<(usize, String)> = supervisor
        .services
        .borrow()
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.app_id.starts_with("system."))
        .map(|(i, s)| (i, s.name.clone()))
        .collect();
    for (i, name) in current {
        let gone = !listed.iter().any(|(s, _)| s.name == name);
        if !gone || supervisor.removed.borrow().contains(&i) {
            continue;
        }
        supervisor.held.borrow_mut().insert(i);
        if let Some(pid) = supervisor.pid(i) {
            supervisor.stop(i, pid).await;
        }
        supervisor.dir.withdraw(&name);
        supervisor.removed.borrow_mut().insert(i);
        mikros_rt::log!(Info, "the service {name} of a package was removed");
    }
    let mut new = Vec::new();
    for (service, manifest) in listed {
        let Some(i) = supervisor.position(&service.name) else {
            new.push((service, manifest));
            continue;
        };
        if !supervisor.removed.borrow_mut().remove(&i) {
            continue;
        }
        // Back after it was removed.
        supervisor.held.borrow_mut().remove(&i);
        let boot = service.start == Start::Boot;
        supervisor.services.borrow_mut()[i] = service.clone();
        supervisor.manifests.borrow_mut()[i] = manifest;
        supervisor
            .dir
            .add_services(i, core::slice::from_ref(&service), |_| {
                if boot {
                    Initially::Starting
                } else {
                    Initially::Idle
                }
            });
        if boot && !supervisor.stopping.get() {
            supervisor.start_with_after(i, Vec::new()).await;
        }
    }
    crate::root::start_services(supervisor, new).await;
    Ok(())
}
