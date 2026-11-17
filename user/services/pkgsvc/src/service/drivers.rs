//! Installed drivers (MIKSPEC-0101.R19, MIKSPEC-0101.R20): a driver package
//! holds the driver manifest `driver.yaml`, named as the package, which
//! asks for no permission beyond those an installed driver may have; and
//! the boot list `boot-modules` of the subvolume `drivers` names the
//! installed drivers marked for boot, written again, and a new try of the
//! boot started, whenever an install or a removal changes it.

use super::files::Root;
use super::{FOREVER, Shared};
use mikros_mika::format::Package;
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::boot::{Attempts, AttemptsClient};

/// The driver manifest's file in a driver's package and folder.
const MANIFEST: &str = "driver.yaml";
/// The subvolume of installed drivers, and its boot list, from the root.
const DRIVERS: &str = "drivers";
const BOOT_MODULES: &str = "drivers/boot-modules";
/// The only `system` permissions that an installed driver may have; it
/// may have no `admin` one.
const ALLOWED: [&str; 2] = ["system.permissions.read", "system.filesystems.mount"];
const ATTEMPTS: &str = "mikros.boot.Attempts";
/// The first line of the boot list.
const HEADER: &str = "# The installed drivers that the bootloader loads, written by pkgsvc.\n";

/// Checks the driver manifest of the driver package `package`, which
/// `manifest` describes: `INVALID_ARGS` when it is missing, does not
/// name the package, or asks for a permission that an installed driver
/// may not have.
pub fn check<S: mikros_mika::format::Source + ?Sized>(
    package: &Package,
    given: &S,
    manifest: &Manifest,
) -> Result<(), Status> {
    if manifest.driver.as_deref() != Some(MANIFEST) {
        return Err(Status::InvalidArgs);
    }
    let entry = package.find(MANIFEST).ok_or(Status::InvalidArgs)?;
    let bytes = package
        .read(given, entry)
        .map_err(|_| Status::InvalidArgs)?;
    let driver = mikros_manifest::parse_driver(&manifest.id, &bytes)
        .map_err(|_| Status::InvalidArgs)?
        .driver;
    let forbidden = driver.permissions.iter().find(|p| {
        (p.starts_with("system.") && !ALLOWED.contains(&p.as_str())) || p.starts_with("admin.")
    });
    if let Some(permission) = forbidden {
        eprintln!("pkgsvc: the driver {} asks for {permission}", manifest.id);
        return Err(Status::InvalidArgs);
    }
    Ok(())
}

/// The boot list of the installed drivers marked for boot: a line
/// `<id>/` for each, in ID order.
fn boot_list(root: &Root) -> Result<String, Status> {
    let mut ids = match root.folders(DRIVERS) {
        Ok(ids) => ids,
        Err(Status::NotFound) => Vec::new(),
        Err(e) => return Err(e),
    };
    ids.sort();
    let mut list = String::from(HEADER);
    for id in ids.iter().filter(|id| mikros_mika::id::is_valid(id)) {
        let Ok(bytes) = root.read(&format!("{DRIVERS}/{id}/{MANIFEST}")) else {
            continue;
        };
        if mikros_manifest::parse_driver(id, &bytes).is_ok_and(|m| m.driver.boot) {
            list.push_str(&format!("{id}/\n"));
        }
    }
    Ok(list)
}

/// Writes the boot list again when it changed, committed before it
/// returns, and then starts a new try of the boot, so that a boot that
/// fails with the new list falls back.
pub fn boot_modules_changed(shared: &Shared, root: &Root) -> Result<(), Status> {
    let list = boot_list(root)?;
    let old = root.read(BOOT_MODULES).ok();
    let empty = list == HEADER;
    if old.as_deref() == Some(list.as_bytes()) || (old.is_none() && empty) {
        return Ok(());
    }
    root.write_file(BOOT_MODULES, list.as_bytes(), 0o644)?;
    start_trying(shared)
}

/// `StartTrying` of the boot attempts service.
fn start_trying(shared: &Shared) -> Result<(), Status> {
    let (client, server) = endpoints::<Attempts>().map_err(|_| Status::NoResources)?;
    {
        let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
        match directory.connect(ATTEMPTS.into(), None, server.into_handle(), FOREVER) {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::Io),
        }
    }
    match AttemptsClient::new(client).start_trying(FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::Io),
    }
}
