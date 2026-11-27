//! Grant lists: the `permissions.yaml` at the root of a system app's
//! package, and the rules that its entries and the `admin` and `user`
//! entries of program manifests obey.

use crate::{Error, ErrorKind, error, list};
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

/// The file name of a grant list in a package.
pub const GRANTS_FILE: &str = "permissions.yaml";
/// The most names that a grant list or a manifest's `permissions` may hold.
pub const MAX_GRANTS: usize = 32;
/// The `system` permissions that a grant list may hold.
const LIST_SYSTEM: [&str; 3] = [
    "system.prompts.use",
    "system.permissions.request",
    "system.gpu_priority.use",
];
/// The permissions that neither a grant list nor a program manifest's
/// `admin` and `user` entries may hold.
const RESERVED: [&str; 11] = [
    "user.full_disk.access",
    "user.desktop.access",
    "user.documents.access",
    "user.downloads.access",
    "user.pictures.access",
    "user.music.access",
    "user.videos.access",
    "user.developer.access",
    "user.volumes.access",
    "user.network_volumes.access",
    "admin.grants.manage",
];
/// What a greeter's grant list may hold of the `user` scope.
const GREETER_USER: &str = "user.power.control";
const AUTHENTICATE: &str = "system.users.authenticate";

/// Where a permission name is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The grant list of a system app, which `greeter` says provides the
    /// role `greeter`.
    List { greeter: bool },
    /// A program manifest's `permissions`, whose `system` entries have no
    /// further rule.
    Program,
}

/// Why an entry may not be registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// The name is not `<scope>.<thing>.<verb>` in the form that
    /// permission names have.
    BadName,
    /// The name is in the reserved set.
    Reserved,
    /// A grant list holds a `system` permission other than the three it
    /// may hold, or a name of a scope other than `system`, `admin` and
    /// `user`.
    Scope,
    /// A greeter's grant list holds what a greeter may not.
    Greeter,
}

/// A grant list: the entries that may be registered, and those that may
/// not, with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrantList {
    pub names: Vec<String>,
    pub rejected: Vec<(String, Rejected)>,
}

/// Whether `name` has the form of a permission name.
fn is_permission_name(name: &str) -> bool {
    let part = |p: &str| {
        let mut chars = p.chars();
        chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    name.len() <= 64 && {
        let mut parts = name.split('.');
        let ok = parts.by_ref().take(3).filter(|p| part(p)).count() == 3;
        ok && parts.next().is_none() && name.matches('.').count() == 2
    }
}

/// Whether the entry `name` of a list or manifest from `source` may be
/// registered; program manifests are only asked about `admin` and `user`
/// entries.
pub fn check_grant(name: &str, source: Source) -> Result<(), Rejected> {
    if !is_permission_name(name) {
        return Err(Rejected::BadName);
    }
    if RESERVED.contains(&name) {
        return Err(Rejected::Reserved);
    }
    let scope = name.split('.').next().unwrap_or_default();
    match (source, scope) {
        (Source::Program, _) => Ok(()),
        (Source::List { greeter }, "admin") if greeter => Err(Rejected::Greeter),
        (Source::List { greeter }, "user") if greeter && name != GREETER_USER => {
            Err(Rejected::Greeter)
        }
        (Source::List { greeter }, "system") if greeter && name == AUTHENTICATE => {
            Err(Rejected::Greeter)
        }
        (Source::List { .. }, "admin" | "user") => Ok(()),
        (Source::List { .. }, "system") if LIST_SYSTEM.contains(&name) => Ok(()),
        (Source::List { .. }, _) => Err(Rejected::Scope),
    }
}

/// Reads the grant list in `bytes`, the file `permissions.yaml`. A list
/// of more than `MAX_GRANTS` names is an error; entries that break the
/// rules for `source` go to `rejected`, in order, and the others to
/// `names`.
pub fn parse_grant_list(bytes: &[u8], source: Source) -> Result<GrantList, Error> {
    let root = mikros_yaml::parse(bytes)?;
    let map = root.map()?;
    let items = list(map, "permissions")?;
    if items.len() > MAX_GRANTS {
        return Err(error(&root, ErrorKind::TooManyPermissions));
    }
    let mut grants = GrantList::default();
    for item in items {
        let name = item.string()?;
        match check_grant(name, source) {
            Ok(()) => grants.names.push(name.into()),
            Err(why) => grants.rejected.push((name.into(), why)),
        }
    }
    Ok(grants)
}

/// Why the install service refuses a package for its grant list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The file is not a valid grant list.
    Invalid,
    /// The package has the file, and is not a system app or an update of
    /// one.
    NotSystemApp,
    /// The package is an update of a system app, and its set of names is
    /// not that of the base copy.
    Differs,
}

/// Every name of the list in `bytes`, those that may not be registered
/// too.
fn all_names(bytes: Option<&[u8]>) -> Result<BTreeSet<String>, Refused> {
    let Some(bytes) = bytes else {
        return Ok(BTreeSet::new());
    };
    let list =
        parse_grant_list(bytes, Source::List { greeter: false }).map_err(|_| Refused::Invalid)?;
    let rejected = list.rejected.into_iter().map(|(name, _)| name);
    Ok(list.names.into_iter().chain(rejected).collect())
}

/// Whether the install service may install a package whose grant list is
/// `new`, the bytes of its `permissions.yaml` or none. `base` is `None`
/// when the package is not a system app or an update of one, and
/// otherwise the base copy's list, or `Some(None)` when it has none
/// (MIKSPEC-0101.R87).
pub fn check_install(new: Option<&[u8]>, base: Option<Option<&[u8]>>) -> Result<(), Refused> {
    let names = all_names(new)?;
    match base {
        None if new.is_some() => Err(Refused::NotSystemApp),
        None => Ok(()),
        Some(base) if all_names(base)? == names => Ok(()),
        Some(_) => Err(Refused::Differs),
    }
}

/// Whether the app manifest `manifest` (`manifest.yaml`) names `role` in
/// the `roles` of one of its `shell_parts`; a manifest that cannot be
/// read names none.
pub fn names_role(manifest: &[u8], role: &str) -> bool {
    let Ok(root) = mikros_yaml::parse(manifest) else {
        return false;
    };
    let named = || -> Result<bool, mikros_yaml::Error> {
        let mut found = false;
        for part in list(root.map()?, "shell_parts")? {
            for item in list(part.map()?, "roles")? {
                found |= item.string()? == role;
            }
        }
        Ok(found)
    };
    named().unwrap_or(false)
}
