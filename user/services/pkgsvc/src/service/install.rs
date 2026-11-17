//! `Install`: a package is written into a new subvolume next to where it
//! is installed, which then takes the place of the old version, if one is
//! installed (MIKSPEC-0101.R36). A global install asks the broker for the
//! permission of the package's kind first (MIKSPEC-0101.R43). A package
//! with a command that a program in `/system/bin` has is refused
//! (MIKSPEC-0101.R44). The components that `Install` names, and those
//! of the package that are installed, are installed after it, each in
//! the place its own manifest names (MIKSPEC-0101.R59).

use super::files::{Root, user_name};
use super::{Given, Shared, check, open};
use crate::place::{self, Scope};
use crate::records::{Owner, Record};
use mikros_mika::format::{self, Kind as EntryKind, Package};
use mikros_mika::manifest::{Kind, Manifest};
use mikros_mika::version::Version;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{NodeClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::packages::InstallerInstallRequest;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The folder of a library's headers, which an install leaves out unless
/// it is for development (MIKSPEC-0101.R21).
const INCLUDE: &str = "include";
/// The folder of the system's programs, from the root.
const SYSTEM_BIN: &str = "system/bin";

/// The permission that installing, updating or removing a package of
/// any kind for every user needs (MIKSPEC-0037.R37).
pub const PERMISSION: &str = "admin.packages.install";

/// The scope, and its owner in the records, that `global` names for
/// `caller`.
pub fn scope(
    shared: &Shared,
    caller: &ClientIdentity,
    global: bool,
) -> Result<(Scope, Owner), Status> {
    if global {
        return Ok((Scope::Global, Owner::Global));
    }
    let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
    let name = user_name(&directory, caller.user)?;
    Ok((Scope::Local(name), Owner::User(caller.user)))
}

/// The version of the package `id` of `kind` installed in `scope`, if
/// one is.
pub fn installed(root: &Root, kind: Kind, id: &str, scope: &Scope) -> Option<Version> {
    let at = place::subvolume(kind, id, scope)?;
    let bytes = root.read(&format!("{at}/{}", format::MANIFEST)).ok()?;
    Manifest::parse(&bytes).ok()?.version
}

/// Whether a program of the system in `/system/bin` is named `name`.
fn system_has(root: &Root, name: &str) -> bool {
    root.open(
        &format!("{SYSTEM_BIN}/{name}"),
        OpenFlags(0),
        0,
        Rights::READ,
    )
    .is_ok()
}

/// Which installed package of any kind has `id` in `scope`, and its
/// version.
fn installed_any(root: &Root, id: &str, scope: &Scope) -> Option<Version> {
    [Kind::App, Kind::Library, Kind::Driver]
        .into_iter()
        .find_map(|kind| installed(root, kind, id, scope))
}

/// A component (MIKSPEC-0101.R59) of a package being installed, read
/// from its file in the package, and where it goes.
struct Component {
    bytes: Vec<u8>,
    package: Package,
    manifest: Manifest,
    global: bool,
}

/// The component at `path` in `package`, whose manifest is `owner`:
/// `INVALID_ARGS` unless it is a package whose ID starts with the
/// package's ID and `.`.
fn component(
    package: &Package,
    given: &Given,
    owner: &Manifest,
    path: &str,
) -> Result<Component, Status> {
    let entry = package.find(path).ok_or(Status::InvalidArgs)?;
    let bytes = package
        .read(given, entry)
        .map_err(|_| Status::InvalidArgs)?;
    let (inner, manifest, _) = check(&bytes[..])?;
    if !manifest.id.starts_with(&format!("{}.", owner.id)) {
        return Err(Status::InvalidArgs);
    }
    let global = manifest.global;
    Ok(Component {
        package: inner,
        bytes,
        manifest,
        global,
    })
}

/// What one install writes: a package or a component, with its owner,
/// the owner's records scope, when it is a component.
struct One<'a, S: format::Source + ?Sized> {
    source: &'a S,
    package: &'a Package,
    manifest: &'a Manifest,
    global: bool,
    owner_of: Option<(&'a str, Owner)>,
    dependency: bool,
    dev: bool,
    commands: &'a [String],
}

pub fn install(
    shared: &Shared,
    caller: &ClientIdentity,
    q: InstallerInstallRequest,
) -> Result<(), Status> {
    let given = Given(NodeClient::new(q.package));
    let (package, manifest, _) = open(&given)?;
    // The components it names, and those of it that are installed, which
    // are updated with it, all checked before anything is written.
    let (_, owner) = scope(shared, caller, q.global)?;
    let records = shared.root()?.records()?;
    let mut components = Vec::new();
    for path in &manifest.components {
        let named = q.components.iter().any(|c| c == path);
        let component = match component(&package, &given, &manifest, path) {
            Ok(component) => component,
            Err(e) if named => return Err(e),
            Err(_) => continue,
        };
        let installed = records.0.iter().any(|((s, id), r)| {
            *id == component.manifest.id
                && r.owner.as_deref() == Some(manifest.id.as_str())
                && r.owner_in.unwrap_or(*s) == owner
        });
        if named || installed {
            components.push(component);
        }
    }
    if let Some(path) = q
        .components
        .iter()
        .find(|c| !manifest.components.contains(c))
    {
        eprintln!("pkgsvc: {} has no component {path}", manifest.id);
        return Err(Status::InvalidArgs);
    }
    let one = One {
        source: &given,
        package: &package,
        manifest: &manifest,
        global: q.global,
        owner_of: None,
        dependency: q.dependency,
        dev: q.dev,
        commands: &q.commands,
    };
    if !install_one(shared, caller, one)? {
        return Err(Status::AccessDenied);
    }
    for c in &components {
        let one = One {
            source: &c.bytes[..],
            package: &c.package,
            manifest: &c.manifest,
            global: c.global,
            owner_of: Some((&manifest.id, owner)),
            dependency: false,
            dev: q.dev,
            commands: &[],
        };
        // One that needs a permission that is not granted is left out.
        if !install_one(shared, caller, one)? {
            eprintln!("pkgsvc: {} is left out", c.manifest.id);
        }
    }
    super::kept::clean(shared, &*shared.root()?)
}

/// Installs what `one` describes, once checked: false, with nothing
/// written, when it needs `admin.packages.install`, which is not
/// granted.
fn install_one<S: format::Source + ?Sized>(
    shared: &Shared,
    caller: &ClientIdentity,
    one: One<'_, S>,
) -> Result<bool, Status> {
    let (package, manifest) = (one.package, one.manifest);
    let kind = manifest.kind.ok_or(Status::InvalidArgs)?;
    if kind == Kind::Driver {
        super::drivers::check(package, one.source, manifest)?;
    }
    super::services::check(package, one.source, manifest)?;
    if !manifest.installable {
        return Err(Status::InvalidArgs);
    }
    if kind == Kind::Driver && !one.global {
        return Err(Status::InvalidArgs);
    }
    let root = shared.root()?;
    // A command of the system's keeps its name (MIKSPEC-0101.R44).
    if let Some(bin) = manifest.bins.iter().find(|b| system_has(&root, &b.name)) {
        eprintln!(
            "pkgsvc: {} has the command {} of the system",
            manifest.id, bin.name
        );
        return Err(Status::AlreadyExists);
    }
    let (scope, owner) = scope(shared, caller, one.global)?;
    // Not for every user and for this user at once.
    let other = match &scope {
        Scope::Global => scope_of_caller(shared, caller).ok(),
        Scope::Local(_) => Some(Scope::Global),
    };
    if let Some(other) = other
        && installed_any(&root, &manifest.id, &other).is_some()
    {
        return Err(Status::AlreadyExists);
    }
    // Every package it needs, in a version of its range.
    for (needed, range) in &manifest.needs {
        let local = scope_of_caller(shared, caller).ok();
        let found = [Some(Scope::Global), local]
            .into_iter()
            .flatten()
            .filter_map(|s| installed_any(&root, needed, &s))
            .any(|version| range.contains(&version));
        if !found {
            eprintln!(
                "pkgsvc: {} needs {needed}, which is not installed",
                manifest.id
            );
            return Err(Status::NotFound);
        }
    }
    if scope == Scope::Global && !super::allowed(shared, caller, PERMISSION, manifest, "install")? {
        return Ok(false);
    }
    let at = place::subvolume(kind, &manifest.id, &scope).ok_or(Status::InvalidArgs)?;
    // What the scope's other packages have of its commands.
    let others = super::commands::others(&root, &scope, &manifest.id)?;
    // An old version of a library that a program still needs is kept
    // (MIKSPEC-0101.R51).
    let old = installed(&root, kind, &manifest.id, &scope);
    let old_manifest = old.as_ref().and_then(|_| {
        let at = place::subvolume(kind, &manifest.id, &scope)?;
        Manifest::parse(&root.read(&format!("{at}/{}", format::MANIFEST)).ok()?).ok()
    });
    let keep = match (kind, &scope, old) {
        (Kind::Library, Scope::Global, Some(old)) => {
            let new = super::kept::package_files(package, one.dev);
            super::kept::keep_old(shared, &root, &manifest.id, &at, &old.to_string(), &new)?
        }
        _ => None,
    };
    write(
        &root,
        shared,
        one.source,
        package,
        &at,
        place::read_only(kind),
        one.dev,
        keep.as_deref(),
    )?;
    let mut records = root.records()?;
    records.set(
        owner,
        &manifest.id,
        Record {
            owner: one.owner_of.map(|(id, _)| id.into()),
            owner_in: one.owner_of.map(|(_, scope)| scope),
            dependency: one.dependency,
        },
    );
    root.write_records(&records)?;
    let names: Vec<String> = manifest.bins.iter().map(|b| b.name.clone()).collect();
    super::commands::installed(&root, owner, &manifest.id, &names, one.commands, &others)?;
    // A driver marked for boot changes the boot list (MIKSPEC-0101.R20).
    if kind == Kind::Driver {
        super::drivers::boot_modules_changed(shared, &root)?;
    }
    // The services of global packages run from their own folders, and
    // the service manager learns of them (MIKSPEC-0101.R55).
    let old_services = old_manifest
        .as_ref()
        .is_some_and(|m| !m.services.is_empty());
    if scope == Scope::Global && (!manifest.services.is_empty() || old_services) {
        if !manifest.services.is_empty() {
            super::services::make_folders(&root, &manifest.id)?;
        }
        super::services::changed(shared);
    }
    Ok(true)
}

/// The scope of the caller's own user.
fn scope_of_caller(shared: &Shared, caller: &ClientIdentity) -> Result<Scope, Status> {
    scope(shared, caller, false).map(|(scope, _)| scope)
}

#[allow(clippy::too_many_arguments)]
/// Writes the files of `package`, without `include/` unless `dev`, into a
/// new subvolume next to `at`, which then takes the place of `at`.
fn write<S: format::Source + ?Sized>(
    root: &Root,
    shared: &Shared,
    given: &S,
    package: &Package,
    at: &str,
    read_only: bool,
    dev: bool,
    keep: Option<&str>,
) -> Result<(), Status> {
    let (parent, name) = at.rsplit_once('/').ok_or(Status::InvalidArgs)?;
    root.make_folders(parent)?;
    let new = format!("{parent}/.new-{name}");
    let subvolumes = shared.subvolumes()?;
    // What an install that failed half-way left.
    let _ = subvolumes.delete(&new);
    subvolumes.create(&new)?;
    let written = (|| {
        for entry in &package.entries {
            // Headers only for those who build against the package.
            let header = entry.path == INCLUDE || entry.path.starts_with("include/");
            if header && !dev {
                continue;
            }
            let path = format!("{new}/{}", entry.path);
            match entry.kind {
                EntryKind::Folder => root.make_folders(&path)?,
                EntryKind::File => {
                    let contents = package
                        .read(given, entry)
                        .map_err(|_| Status::InvalidArgs)?;
                    let mode = if entry.executable() { 0o755 } else { 0o644 };
                    root.write_file(&path, &contents, mode)?;
                }
                EntryKind::Symlink => {
                    let target = package
                        .read(given, entry)
                        .map_err(|_| Status::InvalidArgs)?;
                    let target = String::from_utf8(target).map_err(|_| Status::InvalidArgs)?;
                    root.symlink(&path, &target)?;
                }
            }
        }
        if read_only {
            subvolumes.set_read_only(&new, true)?;
        }
        root.replace(parent, &format!(".new-{name}"), name, &subvolumes, keep)
    })();
    if written.is_err() {
        let _ = subvolumes.delete(&new);
    }
    written
}
