//! The grant list of a system app (MIKSPEC-0101.R87): a package may have
//! `permissions.yaml` only when it is a system app's update, and then its
//! set of names is that of the base copy's file.

use super::files::Root;
use mikros_manifest::{GRANTS_FILE, check_install};
use mikros_mika::format::{Package, Source};
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{OpenFlags, Rights};

/// The folder of the base copies of system apps, from the root, and the
/// start of their IDs.
const BASE_COPIES: &str = "system/apps";
const PREFIX: &str = "system.mikros.";
/// The manifest that every base copy has.
const MANIFEST: &str = "manifest.yaml";

/// Checks the grant list of the package `package`, which `manifest`
/// describes, a component of another package when `component`:
/// `INVALID_ARGS` when it has the file and is neither a system app nor an
/// update of one, or when it is an update whose set of names differs from
/// its base copy's.
pub fn check<S: Source + ?Sized>(
    root: &Root,
    package: &Package,
    given: &S,
    manifest: &Manifest,
    component: bool,
) -> Result<(), Status> {
    let new = match package.find(GRANTS_FILE) {
        Some(entry) => Some(
            package
                .read(given, entry)
                .map_err(|_| Status::InvalidArgs)?,
        ),
        None => None,
    };
    let base = match component {
        true => None,
        false => base_copy(root, &manifest.id)?,
    };
    let base = base.as_ref().map(|list| list.as_deref());
    check_install(new.as_deref(), base).map_err(|why| {
        eprintln!(
            "pkgsvc: the grant list of {} is refused: {why:?}",
            manifest.id
        );
        Status::InvalidArgs
    })
}

/// The grant list of the base copy of `id`: none when `id` has no base
/// copy, and `Some(None)` when its base copy has no list.
fn base_copy(root: &Root, id: &str) -> Result<Option<Option<Vec<u8>>>, Status> {
    if !id.starts_with(PREFIX) {
        return Ok(None);
    }
    let folder = format!("{BASE_COPIES}/{id}");
    match root.open(
        &format!("{folder}/{MANIFEST}"),
        OpenFlags(0),
        0,
        Rights::READ,
    ) {
        Ok(_) => {}
        Err(Status::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    }
    match root.read(&format!("{folder}/{GRANTS_FILE}")) {
        Ok(bytes) => Ok(Some(Some(bytes))),
        Err(Status::NotFound) => Ok(Some(None)),
        Err(e) => Err(e),
    }
}
