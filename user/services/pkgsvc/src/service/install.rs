//! `Install`: a package is written into a new subvolume next to where it
//! is installed, which then takes the place of the old version, if one is
//! installed (MIKSPEC-0101.R36), for every user or for the caller's user
//! as its manifest's `install` says (MIKSPEC-0101.R61). A global install asks the broker for the
//! permission of the package's kind first (MIKSPEC-0101.R43). A package
//! with a command that a program in `/system/bin` has is refused
//! (MIKSPEC-0101.R44). Its required components, the optional ones that
//! `Install` names and those of it that are installed are installed after
//! it, each in the place its own manifest names, and everything is
//! checked, and the permission asked for once, before anything is
//! written (MIKSPEC-0101.R64, MIKSPEC-0101.R65).

use super::files::{Root, Txn, user_name};
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
pub(super) fn installed_any(root: &Root, id: &str, scope: &Scope) -> Option<Version> {
    [Kind::App, Kind::Library, Kind::Driver]
        .into_iter()
        .find_map(|kind| installed(root, kind, id, scope))
}

/// A component (MIKSPEC-0101.R65) of a package being installed, read
/// from its file in the package, and where it goes.
pub(super) struct Component {
    pub(super) bytes: Vec<u8>,
    pub(super) package: Package,
    pub(super) manifest: Manifest,
    optional: bool,
}

/// The component at `path` in `package`, whose manifest is `owner`:
/// `INVALID_ARGS` unless it is a package whose ID starts with the
/// package's ID and `.`.
pub(super) fn component(
    package: &Package,
    given: &Given,
    owner: &Manifest,
    path: &str,
    optional: bool,
) -> Result<Component, Status> {
    let entry = package.find(path).ok_or(Status::InvalidArgs)?;
    let bytes = package
        .read(given, entry)
        .map_err(|_| Status::InvalidArgs)?;
    let (inner, manifest, _) = check(&bytes[..])?;
    if !manifest.id.starts_with(&format!("{}.", owner.id)) {
        return Err(Status::InvalidArgs);
    }
    Ok(Component {
        package: inner,
        bytes,
        manifest,
        optional,
    })
}

/// What one install writes: a package or a component, with its owner,
/// the owner's records scope, when it is a component.
pub(super) struct One<'a, S: format::Source + ?Sized> {
    pub(super) source: &'a S,
    pub(super) package: &'a Package,
    pub(super) manifest: &'a Manifest,
    pub(super) owner_of: Option<(&'a str, Owner)>,
    pub(super) dependency: bool,
    pub(super) dev: bool,
    pub(super) commands: &'a [String],
}

/// Where a checked install goes.
pub(super) struct Checked {
    kind: Kind,
    pub(super) scope: Scope,
    owner: Owner,
}

pub fn install(
    shared: &Shared,
    caller: &ClientIdentity,
    q: InstallerInstallRequest,
) -> Result<(), Status> {
    let given = Given(NodeClient::new(q.package));
    let (package, manifest, _) = open(&given)?;
    if let Some(path) = q
        .components
        .iter()
        .find(|c| !manifest.components.iter().any(|m| &m.path == *c))
    {
        eprintln!("pkgsvc: {} has no component {path}", manifest.id);
        return Err(Status::InvalidArgs);
    }
    let (_, owner) = scope(shared, caller, manifest.global)?;
    let root = shared.root()?;
    let mut all = Vec::new();
    for c in &manifest.components {
        all.push(component(&package, &given, &manifest, &c.path, c.optional)?);
    }
    let package_one = One {
        source: &given,
        package: &package,
        manifest: &manifest,
        owner_of: None,
        dependency: q.dependency,
        dev: q.dev,
        commands: &q.commands,
    };
    let of_component = |c| one_of(c, (&manifest.id, owner), q.dev);
    let plan = |root: &Root| -> Result<Plan, Status> {
        // Every required component, the optional ones it names and those
        // of it that are installed, which are updated with it
        // (MIKSPEC-0101.R65); the others are left out, and their files
        // kept (MIKSPEC-0101.R71).
        let records = root.records()?;
        let (mut taken, mut left_out) = (Vec::new(), Vec::new());
        for (c, m) in all.iter().zip(&manifest.components) {
            let named = q.components.contains(&m.path);
            let installed = records.0.iter().any(|((s, id), r)| {
                *id == c.manifest.id
                    && r.owner.as_deref() == Some(manifest.id.as_str())
                    && r.owner_in.unwrap_or(*s) == owner
            });
            match !c.optional || named || installed {
                true => taken.push(c),
                false => left_out.push(c),
            }
        }
        // What this install brings may satisfy what its components need.
        let coming: Vec<(&str, Option<&Version>)> = core::iter::once(&manifest)
            .chain(taken.iter().map(|c| &c.manifest))
            .map(|m| (m.id.as_str(), m.version.as_ref()))
            .collect();
        let checked = check_one(shared, caller, root, &package_one, &coming)?;
        let mut components = Vec::new();
        for c in taken {
            components.push((
                c,
                check_one(shared, caller, root, &of_component(c), &coming)?,
            ));
        }
        Ok(Plan {
            checked,
            components,
            left_out,
        })
    };
    // Everything is checked before the question, and before anything is
    // written. admin.packages.install is asked for once for everything
    // that goes for every user, without the change held, as a question
    // may stay open long.
    let asked = plan(&root)?;
    let global = asked.global(&manifest);
    let allowed =
        global.is_empty() || super::request(shared, caller, PERMISSION, &reason(&global))?;
    let granted: Vec<String> = match allowed {
        true => global.iter().map(|m| m.id.clone()).collect(),
        false => Vec::new(),
    };
    // Checked again while the change is held, as another one may have
    // changed what is installed meanwhile; the answer covers only what
    // was asked for. An optional component that goes for every user is
    // left out without it.
    let txn = root.begin()?;
    let Plan {
        checked,
        components,
        left_out,
    } = plan(&txn)?;
    let permitted = |m: &Manifest, c: &Checked| c.scope != Scope::Global || granted.contains(&m.id);
    let needed_and_refused = !permitted(&manifest, &checked)
        || components
            .iter()
            .any(|(c, checked)| !c.optional && !permitted(&c.manifest, checked));
    if needed_and_refused {
        return Err(Status::AccessDenied);
    }
    write_one(shared, &txn, package_one, checked)?;
    let mut kept = left_out;
    for (c, checked) in components {
        if !permitted(&c.manifest, &checked) {
            eprintln!("pkgsvc: the optional {} is left out", c.manifest.id);
            kept.push(c);
            continue;
        }
        write_one(shared, &txn, of_component(c), checked)?;
    }
    super::components::keep(&txn, owner, &manifest.id, &kept)?;
    super::kept::clean(shared, &txn)
}

/// What installing the component `c` of the package `of` writes.
fn one_of<'a>(c: &'a Component, of: (&'a str, Owner), dev: bool) -> One<'a, [u8]> {
    One {
        source: &c.bytes[..],
        package: &c.package,
        manifest: &c.manifest,
        owner_of: Some(of),
        dependency: false,
        dev,
        commands: &[],
    }
}

/// What an install takes: the package as checked, its components that
/// are installed with it as checked, and those that are left out.
struct Plan<'a> {
    checked: Checked,
    components: Vec<(&'a Component, Checked)>,
    left_out: Vec<&'a Component>,
}

impl Plan<'_> {
    /// The manifests among the package's, `manifest`, and those of its
    /// components that go for every user.
    fn global<'m>(&'m self, manifest: &'m Manifest) -> Vec<&'m Manifest> {
        core::iter::once((&self.checked, manifest))
            .chain(
                self.components
                    .iter()
                    .map(|(c, checked)| (checked, &c.manifest)),
            )
            .filter(|(c, _)| c.scope == Scope::Global)
            .map(|(_, m)| m)
            .collect()
    }
}

/// The reason given with the question for `admin.packages.install`: the
/// packages of `global`, which go for every user.
fn reason(global: &[&Manifest]) -> String {
    let names: Vec<String> = global
        .iter()
        .map(|m| format!("{} ({})", m.name.as_deref().unwrap_or(&m.id), m.id))
        .collect();
    format!("install {} for every user", names.join(", "))
}

/// Checks what `one` installs, where the packages of `coming` are about
/// to be installed too, and where it goes.
pub(super) fn check_one<S: format::Source + ?Sized>(
    shared: &Shared,
    caller: &ClientIdentity,
    root: &Root,
    one: &One<'_, S>,
    coming: &[(&str, Option<&Version>)],
) -> Result<Checked, Status> {
    let (package, manifest) = (one.package, one.manifest);
    let kind = manifest.kind.ok_or(Status::InvalidArgs)?;
    if kind == Kind::Driver {
        super::drivers::check(package, one.source, manifest)?;
    }
    super::services::check(package, one.source, manifest)?;
    if !manifest.installable {
        return Err(Status::InvalidArgs);
    }
    if kind == Kind::Driver && !manifest.global {
        return Err(Status::InvalidArgs);
    }
    // A command of the system's keeps its name (MIKSPEC-0101.R44).
    if let Some(bin) = manifest.bins.iter().find(|b| system_has(root, &b.name)) {
        eprintln!(
            "pkgsvc: {} has the command {} of the system",
            manifest.id, bin.name
        );
        return Err(Status::AlreadyExists);
    }
    let (scope, owner) = scope(shared, caller, manifest.global)?;
    // Not for every user and for this user at once.
    let other = match &scope {
        Scope::Global => scope_of_caller(shared, caller).ok(),
        Scope::Local(_) => Some(Scope::Global),
    };
    if let Some(other) = other
        && installed_any(root, &manifest.id, &other).is_some()
    {
        return Err(Status::AlreadyExists);
    }
    // Every package it needs, in a version of its range.
    for (needed, range) in &manifest.needs {
        let local = scope_of_caller(shared, caller).ok();
        let found = [Some(Scope::Global), local]
            .into_iter()
            .flatten()
            .filter_map(|s| installed_any(root, needed, &s))
            .chain(
                coming
                    .iter()
                    .filter(|(id, _)| id == needed)
                    .filter_map(|(_, v)| v.cloned()),
            )
            .any(|version| range.contains(&version));
        if !found {
            eprintln!(
                "pkgsvc: {} needs {needed}, which is not installed",
                manifest.id
            );
            return Err(Status::NotFound);
        }
    }
    Ok(Checked { kind, scope, owner })
}

/// Writes what `one` installs, checked as `checked`.
pub(super) fn write_one<S: format::Source + ?Sized>(
    shared: &Shared,
    root: &Txn<'_>,
    one: One<'_, S>,
    checked: Checked,
) -> Result<(), Status> {
    let (package, manifest) = (one.package, one.manifest);
    let Checked { kind, scope, owner } = checked;
    let at = place::subvolume(kind, &manifest.id, &scope).ok_or(Status::InvalidArgs)?;
    // What the scope's other packages have of its commands.
    let others = super::commands::others(root, &scope, &manifest.id)?;
    // An old version of a library that a program still needs is kept
    // (MIKSPEC-0101.R51).
    let old = installed(root, kind, &manifest.id, &scope);
    let old_manifest = old.as_ref().and_then(|_| {
        Manifest::parse(&root.read(&format!("{at}/{}", format::MANIFEST)).ok()?).ok()
    });
    let keep = match (kind, &scope, old) {
        (Kind::Library, Scope::Global, Some(old)) => {
            let new = super::kept::package_files(package, one.dev);
            super::kept::keep_old(shared, root, &manifest.id, &at, &old.to_string(), &new)?
        }
        _ => None,
    };
    write(
        root,
        shared,
        one.source,
        package,
        &at,
        place::read_only(kind),
        one.dev,
        keep.as_deref(),
    )?;
    let record = Record {
        owner: one.owner_of.map(|(id, _)| id.into()),
        owner_in: one.owner_of.map(|(_, scope)| scope),
        dependency: one.dependency,
    };
    root.update_records(|records| records.set(owner, &manifest.id, record))?;
    let names: Vec<String> = manifest.bins.iter().map(|b| b.name.clone()).collect();
    super::commands::installed(root, owner, &manifest.id, &names, one.commands, &others)?;
    // A driver marked for boot changes the boot list (MIKSPEC-0101.R20).
    if kind == Kind::Driver {
        super::drivers::boot_modules_changed(shared, root)?;
    }
    // The services of global packages run from their own folders, and
    // the service manager learns of them (MIKSPEC-0101.R55).
    let old_services = old_manifest
        .as_ref()
        .is_some_and(|m| !m.services.is_empty());
    if scope == Scope::Global && (!manifest.services.is_empty() || old_services) {
        if !manifest.services.is_empty() {
            super::services::make_folders(root, &manifest.id)?;
        }
        super::services::changed(shared);
    }
    Ok(())
}

/// The scope of the caller's own user.
pub(super) fn scope_of_caller(shared: &Shared, caller: &ClientIdentity) -> Result<Scope, Status> {
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
