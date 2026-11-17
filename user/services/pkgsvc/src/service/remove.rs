//! `Remove`: a package's subvolume goes, with the components it owns in
//! either place, its public folder and, unless the data is kept, its
//! users' folders of the app (MIKSPEC-0101.R60). A global component of a
//! local package goes only when `admin.packages.install` is granted, and
//! otherwise stays without an owner. A package that another installed package needs
//! stays; once one goes, the packages installed only as dependencies that
//! nothing needs any more go too (MIKSPEC-0101.R29).

use super::Shared;
use super::files::{Root, users};
use super::install::{PERMISSION, installed, scope};
use crate::place::{self, Scope};
use crate::records::Owner;
use mikros_mika::format::MANIFEST;
use mikros_mika::manifest::{Kind, Manifest};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// An installed package: where, of what kind, and its manifest.
struct Installed {
    scope: Scope,
    owner: Owner,
    kind: Kind,
    manifest: Manifest,
}

/// Every installed package, for every user and for each user.
fn all(shared: &Shared, root: &Root) -> Result<Vec<Installed>, Status> {
    let mut scopes = vec![(Scope::Global, Owner::Global)];
    {
        let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
        for (id, name) in users(&directory)? {
            scopes.push((Scope::Local(name), Owner::User(id)));
        }
    }
    let mut found = Vec::new();
    for (scope, owner) in scopes {
        for (folder, kind, below) in place::folders(&scope) {
            for id in root.folders(&folder)? {
                let path = match below {
                    Some(below) => format!("{folder}/{id}/{below}/{MANIFEST}"),
                    None => format!("{folder}/{id}/{MANIFEST}"),
                };
                if let Ok(manifest) = root
                    .read(&path)
                    .and_then(|b| Manifest::parse(&b).map_err(|_| Status::InvalidArgs))
                {
                    found.push(Installed {
                        scope: scope.clone(),
                        owner,
                        kind,
                        manifest,
                    });
                }
            }
        }
    }
    Ok(found)
}

/// Whether a package other than `id` among `installed` needs `id` in a
/// version that is installed.
fn needed(installed: &[Installed], id: &str) -> bool {
    installed
        .iter()
        .filter(|p| p.manifest.id != id)
        .any(|p| p.manifest.needs.iter().any(|(needed, _)| needed == id))
}

pub fn remove(
    shared: &Shared,
    caller: &ClientIdentity,
    id: &str,
    global: bool,
    keep_data: bool,
) -> Result<(), Status> {
    let root = shared.root()?;
    let (scope, owner) = scope(shared, caller, global)?;
    let kind = [Kind::App, Kind::Library, Kind::Driver]
        .into_iter()
        .find(|kind| installed(&root, *kind, id, &scope).is_some())
        .ok_or(Status::NotFound)?;
    let everything = all(shared, &root)?;
    if needed(&everything, id) {
        return Err(Status::BadState);
    }
    if scope == Scope::Global {
        let manifest = everything
            .iter()
            .find(|p| p.manifest.id == id && p.scope == Scope::Global)
            .map(|p| p.manifest.clone())
            .ok_or(Status::NotFound)?;
        if !super::allowed(shared, caller, PERMISSION, &manifest, "remove")? {
            return Err(Status::AccessDenied);
        }
    }
    delete(
        shared,
        caller,
        &root,
        &everything,
        id,
        kind,
        &scope,
        owner,
        keep_data,
    )?;
    // The packages that were there only for others, which nothing needs
    // now, one after the other.
    loop {
        let everything = all(shared, &root)?;
        let records = root.records()?;
        let unneeded = everything.iter().find(|p| {
            records.get(p.owner, &p.manifest.id).dependency && !needed(&everything, &p.manifest.id)
        });
        let Some(p) = unneeded else {
            // Kept libraries that only the removed packages needed go too
            // (MIKSPEC-0101.R51).
            return super::kept::clean(shared, &root);
        };
        delete(
            shared,
            caller,
            &root,
            &everything,
            &p.manifest.id,
            p.kind,
            &p.scope,
            p.owner,
            false,
        )?;
    }
}

/// Deletes the package `id` of `kind` in `scope` and what goes with it.
#[allow(clippy::too_many_arguments)]
fn delete(
    shared: &Shared,
    caller: &ClientIdentity,
    root: &Root,
    everything: &[Installed],
    id: &str,
    kind: Kind,
    scope: &Scope,
    owner: Owner,
    keep_data: bool,
) -> Result<(), Status> {
    let subvolumes = shared.subvolumes()?;
    let mut records = root.records()?;
    for (in_scope, component) in records.components(owner, id) {
        let Some(c) = everything
            .iter()
            .find(|p| p.manifest.id == component && p.owner == in_scope)
        else {
            continue;
        };
        // A global component of a local package needs the permission; it
        // stays, without an owner, when that is not granted.
        let local_owner = matches!(owner, Owner::User(_));
        if in_scope == Owner::Global
            && local_owner
            && !super::allowed(shared, caller, PERMISSION, &c.manifest, "remove")?
        {
            let mut records = root.records()?;
            let mut record = records.get(in_scope, &component);
            record.owner = None;
            record.owner_in = None;
            records.set(in_scope, &component, record);
            root.write_records(&records)?;
            continue;
        }
        delete(
            shared, caller, root, everything, &component, c.kind, &c.scope, in_scope, keep_data,
        )?;
    }
    let at = place::subvolume(kind, id, scope).ok_or(Status::InvalidArgs)?;
    subvolumes.delete(&at)?;
    super::commands::removed(root, owner, id)?;
    // The files of its components that were left out (MIKSPEC-0101.R71).
    super::components::forget(root, owner, id);
    if kind == Kind::Driver {
        super::drivers::boot_modules_changed(shared, root)?;
    }
    // The service manager stops the services of a removed global package
    // (MIKSPEC-0101.R55).
    let had_services = everything
        .iter()
        .any(|p| p.manifest.id == id && p.owner == owner && !p.manifest.services.is_empty());
    if *scope == Scope::Global && had_services {
        super::services::changed(shared);
    }
    let _ = subvolumes.delete(&format!("public/{id}"));
    if kind == Kind::App && !keep_data {
        let homes: Vec<String> = match scope {
            Scope::Global => {
                let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
                users(&directory)?
                    .into_iter()
                    .map(|(_, name)| name)
                    .collect()
            }
            Scope::Local(name) => vec![name.clone()],
        };
        for name in homes {
            let folder = format!("users/{name}/apps/{id}");
            if subvolumes.delete(&folder).is_err() {
                let _ = root.remove_all(&folder);
            }
        }
    }
    records = root.records()?;
    records.set(owner, id, Default::default());
    root.write_records(&records)
}
