//! The services of packages (MIKSPEC-0101.R54, MIKSPEC-0101.R55): those of
//! globally installed packages, named `<id>.<name>`, which the service
//! manager asks for with `Services` and `ServiceManifest` and is told of
//! with `ServicesChanged` (MIKSPEC-0036.R40) when an install or a removal
//! changes them; a package whose service manifest is not one of a
//! package's service is refused.

use super::files::Root;
use super::{FOREVER, Shared};
use crate::place::{self, Scope};
use mikros_mika::format::{MANIFEST, Package};
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::packages::PackageService;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, PackageServices, PackageServicesClient,
};

/// The only caller of `Services` and `ServiceManifest`.
const SERVICE_MANAGER: &str = "system.servicemgr";
const PACKAGE_SERVICES: &str = "mikros.service.PackageServices";
/// The most services one `Services` reply holds, and the longest service
/// manifest that `ServiceManifest` returns.
const SERVICES_MAX: usize = 64;
const MANIFEST_MAX: usize = 16384;

/// Checks the service manifests of `package`, which `manifest` describes:
/// `INVALID_ARGS` for one that is missing or is not a package's service
/// manifest, such as one that asks for a `system` or `admin` permission.
pub fn check<S: mikros_mika::format::Source + ?Sized>(
    package: &Package,
    given: &S,
    manifest: &Manifest,
) -> Result<(), Status> {
    for (key, path) in &manifest.services {
        let entry = package.find(path).ok_or(Status::InvalidArgs)?;
        let bytes = package
            .read(given, entry)
            .map_err(|_| Status::InvalidArgs)?;
        if let Err(e) = mikros_manifest::parse_package_service(&manifest.id, key, &bytes) {
            eprintln!("pkgsvc: the service {key} of {}: {:?}", manifest.id, e.kind);
            return Err(Status::InvalidArgs);
        }
    }
    Ok(())
}

/// Makes the folders of the services of the package `id`,
/// `state/apps/<id>/data` and `cache`, from the root.
pub fn make_folders(root: &Root, id: &str) -> Result<(), Status> {
    root.make_folders(&format!("state/apps/{id}/data"))?;
    root.make_folders(&format!("state/apps/{id}/cache"))
}

/// The services of the globally installed packages, by name, each with
/// its package's ID and the path of its manifest from the root.
fn all(root: &Root) -> Result<Vec<(String, String, String)>, Status> {
    let mut found = Vec::new();
    for (folder, kind, _) in place::folders(&Scope::Global) {
        for id in root.folders(&folder)? {
            if !mikros_mika::id::is_valid(&id) {
                continue;
            }
            let Some(at) = place::subvolume(kind, &id, &Scope::Global) else {
                continue;
            };
            let Ok(bytes) = root.read(&format!("{at}/{MANIFEST}")) else {
                continue;
            };
            let Ok(manifest) = Manifest::parse(&bytes) else {
                continue;
            };
            for (key, path) in manifest.services {
                found.push((format!("{id}.{key}"), id.clone(), format!("{at}/{path}")));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The services of global packages whose names sort after `after`, for
/// the service manager only.
pub fn list(
    shared: &Shared,
    caller: &ClientIdentity,
    after: &str,
) -> Result<Vec<PackageService>, Status> {
    if caller.app_id != SERVICE_MANAGER {
        return Err(Status::AccessDenied);
    }
    let root = shared.root()?;
    Ok(all(&root)?
        .into_iter()
        .filter(|(name, ..)| name.as_str() > after)
        .take(SERVICES_MAX)
        .map(|(name, app_id, _)| PackageService { name, app_id })
        .collect())
}

/// The manifest of the service `name` of a global package, for the
/// service manager only.
pub fn manifest(shared: &Shared, caller: &ClientIdentity, name: &str) -> Result<Vec<u8>, Status> {
    if caller.app_id != SERVICE_MANAGER {
        return Err(Status::AccessDenied);
    }
    let root = shared.root()?;
    let (_, _, path) = all(&root)?
        .into_iter()
        .find(|(n, ..)| n == name)
        .ok_or(Status::NotFound)?;
    let bytes = root.read(&path)?;
    if bytes.len() > MANIFEST_MAX {
        return Err(Status::InvalidArgs);
    }
    Ok(bytes)
}

/// Tells the service manager that the services of packages changed. A
/// failure is only logged, as the install it follows is done.
pub fn changed(shared: &Shared) {
    let told = (|| {
        let (client, server) = endpoints::<PackageServices>().map_err(|_| Status::NoResources)?;
        {
            let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
            match directory.connect(PACKAGE_SERVICES.into(), None, server.into_handle(), FOREVER) {
                Ok(Ok(_)) => {}
                Ok(Err(status)) => return Err(status),
                Err(_) => return Err(Status::Io),
            }
        }
        match PackageServicesClient::new(client).services_changed(FOREVER) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(status)) => Err(status),
            Err(_) => Err(Status::Io),
        }
    })();
    if let Err(status) = told {
        eprintln!("pkgsvc: telling the service manager that services changed failed: {status:?}");
    }
}
