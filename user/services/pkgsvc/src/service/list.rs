//! `List`: the packages installed for every user and for the caller's
//! user, by ID, from their manifests in their subvolumes, with their
//! owners from the records.

use super::Shared;
use super::files::user_name;
use crate::place::{self, Scope};
use crate::records::Owner;
use mikros_mika::format::MANIFEST;
use mikros_mika::manifest::{Kind, Manifest};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::packages::PackageInfo;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The most packages one reply holds.
const LIST_MAX: usize = 64;

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::App => "app",
        Kind::Library => "library",
        Kind::Driver => "driver",
    }
}

/// The installed packages whose IDs come after `after`, at most
/// `LIST_MAX` of them.
pub fn list(
    shared: &Shared,
    caller: &ClientIdentity,
    after: &str,
) -> Result<Vec<PackageInfo>, Status> {
    let root = shared.root()?;
    let records = root.records()?;
    let mut scopes = vec![(Scope::Global, Owner::Global)];
    if caller.user != 0 {
        let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
        let name = user_name(&directory, caller.user)?;
        scopes.push((Scope::Local(name), Owner::User(caller.user)));
    }
    let mut found = Vec::new();
    for (scope, owner) in &scopes {
        for (folder, kind, below) in place::folders(scope) {
            for id in root.folders(&folder)? {
                if id.as_str() <= after || !mikros_mika::id::is_valid(&id) {
                    continue;
                }
                let path = match below {
                    Some(below) => format!("{folder}/{id}/{below}/{MANIFEST}"),
                    None => format!("{folder}/{id}/{MANIFEST}"),
                };
                // A folder without a manifest holds no installed package,
                // as an app's data folder does without its package.
                let Ok(bytes) = root.read(&path) else {
                    continue;
                };
                let Ok(manifest) = Manifest::parse(&bytes) else {
                    continue;
                };
                found.push(PackageInfo {
                    id: id.clone(),
                    kind: kind_name(kind).into(),
                    version: manifest.version.map(|v| v.to_string()).unwrap_or_default(),
                    name: manifest.name.unwrap_or_default(),
                    global: *scope == Scope::Global,
                    owner: records.get(*owner, &id).owner.unwrap_or_default(),
                });
            }
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found.truncate(LIST_MAX);
    Ok(found)
}
