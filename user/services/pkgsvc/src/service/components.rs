//! Components added later (MIKSPEC-0101.R71, MIKSPEC-0101.R72): the
//! files of the optional components that an install leaves out are kept
//! as `state/pkgsvc/components/<scope>/<id>/<component ID>.mika` until
//! the package is updated, which keeps those of its new version, or
//! removed, or the component is installed. `AddComponent` installs a kept
//! one by its ID, and `InstallComponent` a component that a caller gives
//! as a package file.

use super::files::Root;
use super::install::{
    Component, One, PERMISSION, check_one, installed, installed_any, scope, write_one,
};
use super::{Given, Shared};
use crate::place::Scope;
use crate::records::Owner;
use mikros_mika::format::Source;
use mikros_rt::interfaces::__midl::ClientEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient};
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// Where the files of left-out components are kept, from the root.
const KEPT: &str = "state/pkgsvc/components";
/// The permission that installing a component for its user needs.
const LOCAL_PERMISSION: &str = "user.components.install";
/// The largest component file that `InstallComponent` reads.
const PACKAGE_MAX: u64 = 1 << 30;

/// The folder of the kept components of the package `id` of `owner`.
fn folder(owner: Owner, id: &str) -> String {
    match owner {
        Owner::Global => format!("{KEPT}/global/{id}"),
        Owner::User(user) => format!("{KEPT}/user/{user}/{id}"),
    }
}

/// Keeps the files of `left_out`, the optional components of the
/// package `id` of `owner` that are not installed, in place of those kept
/// before.
pub fn keep(root: &Root, owner: Owner, id: &str, left_out: &[&Component]) -> Result<(), Status> {
    let at = folder(owner, id);
    let _ = root.remove_all(&at);
    if left_out.is_empty() {
        return Ok(());
    }
    root.make_folders(&at)?;
    for c in left_out {
        root.write_file(&format!("{at}/{}.mika", c.manifest.id), &c.bytes, 0o644)?;
    }
    Ok(())
}

/// Deletes the kept components of the package `id` of `owner`.
pub fn forget(root: &Root, owner: Owner, id: &str) {
    let _ = root.remove_all(&folder(owner, id));
}

/// Deletes the kept file of the component `component` of the package
/// `id` of `owner`, once it is installed, and the folder once it holds
/// none.
fn installed_now(root: &Root, owner: Owner, id: &str, component: &str) {
    let at = folder(owner, id);
    let _ = root.remove_all(&format!("{at}/{component}.mika"));
    if root.files(&at).is_ok_and(|f| f.is_empty()) {
        let _ = root.remove_all(&at);
    }
}

/// Where the package `id` is installed for `caller`: for every user, or
/// for the caller's user; `NOT_FOUND` when it is not.
fn owner_of(
    shared: &Shared,
    caller: &ClientIdentity,
    root: &Root,
    id: &str,
) -> Result<Owner, Status> {
    if installed_any(root, id, &Scope::Global).is_some() {
        return Ok(Owner::Global);
    }
    let (local, owner) = scope(shared, caller, false)?;
    match installed_any(root, id, &local) {
        Some(_) => Ok(owner),
        None => Err(Status::NotFound),
    }
}

/// Installs the component of the package `id` of `owner` in `bytes`,
/// once checked and once the permission of its place is granted:
/// `admin.packages.install` for every user, `user.components.install`
/// for the caller's user. `fresh` refuses one installed in the same or a
/// newer version with `BAD_STATE`.
fn install_component(
    shared: &Shared,
    caller: &ClientIdentity,
    root: &Root,
    (id, owner): (&str, Owner),
    bytes: &[u8],
    fresh: bool,
) -> Result<(), Status> {
    let (package, manifest, _) = super::check(bytes)?;
    if !manifest.id.starts_with(&format!("{id}.")) {
        return Err(Status::InvalidArgs);
    }
    let kind = manifest.kind.ok_or(Status::InvalidArgs)?;
    let (place, _) = scope(shared, caller, manifest.global)?;
    if fresh
        && let (Some(old), Some(new)) = (
            installed(root, kind, &manifest.id, &place),
            &manifest.version,
        )
        && old >= *new
    {
        return Err(Status::BadState);
    }
    let one = One {
        source: bytes,
        package: &package,
        manifest: &manifest,
        owner_of: Some((id, owner)),
        dependency: false,
        dev: false,
        commands: &[],
    };
    let coming = [(manifest.id.as_str(), manifest.version.as_ref())];
    let checked = check_one(shared, caller, root, &one, &coming)?;
    let name = manifest.name.as_deref().unwrap_or(&manifest.id);
    let (permission, reason) = match checked.scope {
        Scope::Global => (
            PERMISSION,
            format!("install {name} ({}) for every user", manifest.id),
        ),
        Scope::Local(_) => (
            LOCAL_PERMISSION,
            format!("install the component {name} ({})", manifest.id),
        ),
    };
    if !super::request(shared, caller, permission, &reason)? {
        return Err(Status::AccessDenied);
    }
    let component = manifest.id.clone();
    write_one(shared, root, one, checked)?;
    installed_now(root, owner, id, &component);
    super::kept::clean(shared, root)
}

/// `AddComponent`: installs the kept optional component `component`, by
/// its ID, of the package `id`.
pub fn add(
    shared: &Shared,
    caller: &ClientIdentity,
    id: &str,
    component: &str,
) -> Result<(), Status> {
    if !mikros_mika::id::is_valid(component) {
        return Err(Status::InvalidArgs);
    }
    let root = shared.root()?;
    let owner = owner_of(shared, caller, &root, id)?;
    let installed = root
        .records()?
        .components(owner, id)
        .into_iter()
        .any(|(_, c)| c == component);
    if installed {
        return Err(Status::AlreadyExists);
    }
    let bytes = root
        .read(&format!("{}/{component}.mika", folder(owner, id)))
        .map_err(|_| Status::NotFound)?;
    install_component(shared, caller, &root, (id, owner), &bytes, false)
}

/// `InstallComponent`: installs the package that `package` gives as a
/// component of the package `id`, or updates the installed one to it.
pub fn install_given(
    shared: &Shared,
    caller: &ClientIdentity,
    id: &str,
    package: ClientEnd<Node>,
) -> Result<(), Status> {
    let given = Given(NodeClient::new(package));
    let size = given.size();
    if size > PACKAGE_MAX {
        return Err(Status::InvalidArgs);
    }
    let mut bytes = vec![0; size as usize];
    given.read_at(0, &mut bytes).map_err(|_| Status::Io)?;
    let root = shared.root()?;
    let owner = owner_of(shared, caller, &root, id)?;
    install_component(shared, caller, &root, (id, owner), &bytes, true)
}
