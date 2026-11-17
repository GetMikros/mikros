//! `Install`: a package is written into a new subvolume next to where it
//! is installed, which then takes the place of the old version, if one is
//! installed (MIKSPEC-0101.R36). A global install asks the broker for the
//! permission of the package's kind first (MIKSPEC-0101.R43). A package
//! with a command that a program in `/system/bin` has is refused
//! (MIKSPEC-0101.R44).

use super::files::{Root, user_name};
use super::{Given, Shared, open};
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

/// The permission that installing a package of `kind` for every user
/// needs.
pub fn permission(kind: Kind) -> &'static str {
    match kind {
        Kind::App => "admin.apps.install",
        Kind::Library => "admin.libraries.install",
        Kind::Driver => "admin.drivers.install",
    }
}

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

pub fn install(
    shared: &Shared,
    caller: &ClientIdentity,
    q: InstallerInstallRequest,
) -> Result<(), Status> {
    let given = Given(NodeClient::new(q.package));
    let (package, manifest, _) = open(&given)?;
    let kind = manifest.kind.ok_or(Status::InvalidArgs)?;
    if kind == Kind::Driver {
        super::drivers::check(&package, &given, &manifest)?;
    }
    if !manifest.installable {
        return Err(Status::InvalidArgs);
    }
    if kind == Kind::Driver && !q.global {
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
    let (scope, owner) = scope(shared, caller, q.global)?;
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
    if scope == Scope::Global
        && !super::allowed(shared, caller, permission(kind), &manifest, "install")?
    {
        return Err(Status::AccessDenied);
    }
    let at = place::subvolume(kind, &manifest.id, &scope).ok_or(Status::InvalidArgs)?;
    // What the scope's other packages have of its commands.
    let others = super::commands::others(&root, &scope, &manifest.id)?;
    // An old version of a library that a program still needs is kept
    // (MIKSPEC-0101.R51).
    let old = installed(&root, kind, &manifest.id, &scope);
    let keep = match (kind, &scope, old) {
        (Kind::Library, Scope::Global, Some(old)) => {
            let new = super::kept::package_files(&package, q.dev);
            super::kept::keep_old(shared, &root, &manifest.id, &at, &old.to_string(), &new)?
        }
        _ => None,
    };
    write(
        &root,
        shared,
        &given,
        &package,
        &at,
        place::read_only(kind),
        q.dev,
        keep.as_deref(),
    )?;
    let mut records = root.records()?;
    records.set(
        owner,
        &manifest.id,
        Record {
            owner: None,
            dependency: q.dependency,
        },
    );
    root.write_records(&records)?;
    let names: Vec<String> = manifest.bins.iter().map(|b| b.name.clone()).collect();
    super::commands::installed(&root, owner, &manifest.id, &names, &q.commands, &others)?;
    // A driver marked for boot changes the boot list (MIKSPEC-0101.R20).
    if kind == Kind::Driver {
        super::drivers::boot_modules_changed(shared, &root)?;
    }
    super::kept::clean(shared, &root)
}

/// The scope of the caller's own user.
fn scope_of_caller(shared: &Shared, caller: &ClientIdentity) -> Result<Scope, Status> {
    scope(shared, caller, false).map(|(scope, _)| scope)
}

#[allow(clippy::too_many_arguments)]
/// Writes the files of `package`, without `include/` unless `dev`, into a
/// new subvolume next to `at`, which then takes the place of `at`.
fn write(
    root: &Root,
    shared: &Shared,
    given: &Given,
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
