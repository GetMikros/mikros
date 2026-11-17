//! `CommandOwner` (MIKSPEC-0101.R43): the commands of installed packages,
//! found in their manifests, and which of them each user sees.

use super::Shared;
use super::files::{Root, user_name};
use crate::commands::{self, Command, Owners, Provided};
use crate::place::{self, Scope};
use crate::records::Owner;
use mikros_mika::format::MANIFEST;
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use std::collections::BTreeMap;

/// The only caller of `CommandOwner`.
const PROCESS_MANAGER: &str = "system.processmgr";
/// The commands that the packages of `scope` have.
pub fn provided(root: &Root, scope: &Scope) -> Result<Provided, Status> {
    let mut found = Provided::new();
    for (folder, kind, _) in place::folders(scope) {
        let ids = match root.folders(&folder) {
            Ok(ids) => ids,
            Err(Status::NotFound) => continue,
            Err(e) => return Err(e),
        };
        for id in ids.into_iter().filter(|id| mikros_mika::id::is_valid(id)) {
            let Some(at) = place::subvolume(kind, &id, scope) else {
                continue;
            };
            let Ok(bytes) = root.read(&format!("{at}/{MANIFEST}")) else {
                continue;
            };
            let Ok(manifest) = Manifest::parse(&bytes) else {
                continue;
            };
            for bin in manifest.bins {
                found.entry(bin.name).or_default().push(Command {
                    id: id.clone(),
                    file: format!("{at}/{}", bin.path),
                });
            }
        }
    }
    Ok(found)
}

/// The commands that `user` sees, or, for `None`, those of every user.
fn visible(shared: &Shared, user: Option<u32>) -> Result<BTreeMap<String, Command>, Status> {
    let root = shared.root()?;
    let owners = root.owners()?;
    let global = owners.resolve(Owner::Global, &provided(&root, &Scope::Global)?);
    let local = match user.filter(|u| *u != 0) {
        Some(user) => {
            let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
            let name = user_name(&directory, user)?;
            drop(directory);
            owners.resolve(Owner::User(user), &provided(&root, &Scope::Local(name))?)
        }
        None => BTreeMap::new(),
    };
    Ok(commands::visible(global, local))
}

/// Records who keeps the commands of `manifest_id`, installed in `scope`
/// for `owner`, that other packages there have: the package itself for
/// those in `taken`. `others` are the commands of the scope's other
/// packages before the install.
pub fn installed(
    root: &Root,
    owner: Owner,
    id: &str,
    names: &[String],
    taken: &[String],
    others: &Provided,
) -> Result<(), Status> {
    let mut owners = root.owners()?;
    let before = owners.encode();
    owners.install(owner, id, names, taken, others);
    match owners.encode() == before {
        true => Ok(()),
        false => root.write_owners(&owners),
    }
}

/// The commands of `scope` but those of the package `id`.
pub fn others(root: &Root, scope: &Scope, id: &str) -> Result<Provided, Status> {
    let mut found = provided(root, scope)?;
    for providers in found.values_mut() {
        providers.retain(|c| c.id != id);
    }
    Ok(found)
}

/// Forgets the commands that the package `id` of `owner` owned.
pub fn removed(root: &Root, owner: Owner, id: &str) -> Result<(), Status> {
    let mut owners: Owners = root.owners()?;
    let before = owners.encode();
    owners.forget(owner, id);
    match owners.encode() == before {
        true => Ok(()),
        false => root.write_owners(&owners),
    }
}

/// The ID of the package whose command `name` the user `user` sees, for
/// the process manager only.
pub fn command_owner(
    shared: &Shared,
    caller: &ClientIdentity,
    name: &str,
    user: u32,
) -> Result<String, Status> {
    if caller.app_id != PROCESS_MANAGER {
        return Err(Status::AccessDenied);
    }
    visible(shared, Some(user))?
        .remove(name)
        .map(|c| c.id)
        .ok_or(Status::NotFound)
}
