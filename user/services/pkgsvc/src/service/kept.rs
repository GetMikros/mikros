//! Kept versions of library packages (MIKSPEC-0101.R51): when an update of
//! a library package removes a library that a program of an installed
//! package names, the old version's subvolume is kept, read-only, as
//! `.kept/<id>/<version>` in the subvolume `libraries`, and it is deleted
//! at the install or removal after which nothing names a library that
//! only it holds.

use super::Shared;
use super::files::{Root, users};
use crate::kept::{keeps, still_needed};
use crate::place::{self, Scope};
use mikros_mika::format::{Kind as EntryKind, Package};
use mikros_rt::interfaces::mikros::base::Status;
use std::collections::BTreeSet;

/// The folder of kept versions, from the root.
const KEPT: &str = "libraries/.kept";
/// The folder of a library package's headers, which are not libraries.
const INCLUDE: &str = "include/";
/// The largest image whose names are read.
const IMAGE_MAX: usize = 1 << 26;

/// The library names that the programs and libraries of every installed
/// package name in `DT_NEEDED`.
pub fn needed(shared: &Shared, root: &Root) -> Result<BTreeSet<String>, Status> {
    let mut scopes = vec![Scope::Global];
    {
        let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
        scopes.extend(
            users(&directory)?
                .into_iter()
                .map(|(_, name)| Scope::Local(name)),
        );
    }
    let mut needed = BTreeSet::new();
    for scope in &scopes {
        for (folder, kind, _) in place::folders(scope) {
            for id in root.folders(&folder)? {
                if !mikros_mika::id::is_valid(&id) {
                    continue;
                }
                let Some(at) = place::subvolume(kind, &id, scope) else {
                    continue;
                };
                for file in root.files(&at)? {
                    let Ok(bytes) = root.read_at_most(&format!("{at}/{file}"), IMAGE_MAX) else {
                        continue;
                    };
                    if let Some(names) = mikros_mika::elf::names(&bytes) {
                        needed.extend(names.needed);
                    }
                }
            }
        }
    }
    Ok(needed)
}

/// The files that installing `package` writes, without its headers
/// unless `dev`.
pub fn package_files(package: &Package, dev: bool) -> BTreeSet<String> {
    package
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File && (dev || !e.path.starts_with(INCLUDE)))
        .map(|e| e.path.clone())
        .collect()
}

/// Where the old version `version` of the global library package `id`,
/// installed at `at`, is kept when installing the files `new` over it
/// removes a library that an installed program names; none otherwise.
pub fn keep_old(
    shared: &Shared,
    root: &Root,
    id: &str,
    at: &str,
    version: &str,
    new: &BTreeSet<String>,
) -> Result<Option<String>, Status> {
    let old: BTreeSet<String> = root.files(at)?.into_iter().collect();
    let needed = needed(shared, root)?;
    Ok(keeps(id, &old, new, &needed).then(|| format!("{KEPT}/{id}/{version}")))
}

/// Deletes the kept versions that no program of an installed package
/// needs any more.
pub fn clean(shared: &Shared, root: &Root) -> Result<(), Status> {
    let ids = root.folders(KEPT)?;
    if ids.is_empty() {
        return Ok(());
    }
    let needed = needed(shared, root)?;
    let subvolumes = shared.subvolumes()?;
    for id in ids {
        let current: BTreeSet<String> = root
            .files(&format!("libraries/{id}"))?
            .into_iter()
            .collect();
        for version in root.folders(&format!("{KEPT}/{id}"))? {
            let at = format!("{KEPT}/{id}/{version}");
            let kept: BTreeSet<String> = root.files(&at)?.into_iter().collect();
            if !still_needed(&id, &kept, &current, &needed) {
                subvolumes.delete(&at)?;
            }
        }
        if root.folders(&format!("{KEPT}/{id}"))?.is_empty() {
            let _ = root.remove_all(&format!("{KEPT}/{id}"));
        }
    }
    Ok(())
}
