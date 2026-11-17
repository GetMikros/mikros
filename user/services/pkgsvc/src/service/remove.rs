//! `Remove`: a package's subvolume goes, with the components it owns in
//! either place, its public folder and, unless the data is kept, its
//! users' folders of the app (MIKSPEC-0101.R60). A global component of a
//! local package goes only when `admin.packages.install` is granted, and
//! otherwise stays without an owner. A package that another installed package needs
//! stays; once one goes, the packages installed only as dependencies that
//! nothing needs any more go too (MIKSPEC-0101.R29).

use super::Shared;
use super::files::{Root, Txn, users};
use super::install::{PERMISSION, installed, scope};
use crate::place::{self, Scope};
use crate::records::{Owner, Records};
use mikros_mika::format::MANIFEST;
use mikros_mika::manifest::{Kind, Manifest};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use std::collections::BTreeMap;

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
    let start = Start {
        id,
        scope: &scope,
        owner,
        keep_data,
    };
    // Planned while the change is held, which a question would hold too
    // long: the plan stops at the first question it has no answer to,
    // which is asked without it, and is then made again with the answer.
    let mut answers = BTreeMap::new();
    loop {
        let question = {
            let txn = root.begin()?;
            match plan(shared, &txn, &start, &answers)? {
                Planned::Steps(steps) => {
                    for step in steps {
                        take(shared, &txn, step)?;
                    }
                    // Kept libraries that only the removed packages
                    // needed go too (MIKSPEC-0101.R51).
                    return super::kept::clean(shared, &txn);
                }
                Planned::Ask(question) => question,
            }
        };
        let allowed = super::allowed(shared, caller, PERMISSION, &question.1, "remove")?;
        answers.insert(question.0, allowed);
    }
}

/// What `Remove` was asked to remove.
struct Start<'a> {
    id: &'a str,
    scope: &'a Scope,
    owner: Owner,
    keep_data: bool,
}

/// A question for `admin.packages.install`: the package or component
/// that it is for, by its place in the records, and its manifest.
type Question = ((Owner, String), Box<Manifest>);

/// What a removal comes to with the answers given so far.
enum Planned {
    /// Everything it does, in order.
    Steps(Vec<Step>),
    /// The first question it has no answer to.
    Ask(Question),
}

/// One thing that a removal does.
enum Step {
    /// The package `id` of `kind` in `scope` goes.
    Delete {
        id: String,
        kind: Kind,
        scope: Scope,
        owner: Owner,
        keep_data: bool,
        had_services: bool,
    },
    /// The global component `id` of a local package stays without an
    /// owner, as the permission to remove it was not granted.
    Keep { owner: Owner, id: String },
}

/// The steps of removing `start`, worked out from what is installed
/// now: `NOT_FOUND` when it is not installed, `BAD_STATE` when another
/// package needs it, `ACCESS_DENIED` when it goes for every user and
/// the permission was not granted.
fn plan(
    shared: &Shared,
    root: &Root,
    start: &Start<'_>,
    answers: &BTreeMap<(Owner, String), bool>,
) -> Result<Planned, Status> {
    let Start {
        id,
        scope,
        owner,
        keep_data,
    } = *start;
    let kind = [Kind::App, Kind::Library, Kind::Driver]
        .into_iter()
        .find(|kind| installed(root, *kind, id, scope).is_some())
        .ok_or(Status::NotFound)?;
    let mut state = State {
        everything: all(shared, root)?,
        records: root.records()?,
        steps: Vec::new(),
    };
    if needed(&state.everything, id) {
        return Err(Status::BadState);
    }
    if *scope == Scope::Global {
        let manifest = state
            .everything
            .iter()
            .find(|p| p.manifest.id == id && p.scope == Scope::Global)
            .map(|p| p.manifest.clone())
            .ok_or(Status::NotFound)?;
        match answers.get(&(Owner::Global, id.into())) {
            None => {
                return Ok(Planned::Ask((
                    (Owner::Global, id.into()),
                    Box::new(manifest),
                )));
            }
            Some(false) => return Err(Status::AccessDenied),
            Some(true) => {}
        }
    }
    if let Err(question) = state.delete(answers, id, kind, scope, owner, keep_data) {
        return Ok(Planned::Ask(question));
    }
    // The packages that were there only for others, which nothing needs
    // now, one after the other.
    loop {
        let unneeded = state.everything.iter().find(|p| {
            state.records.get(p.owner, &p.manifest.id).dependency
                && !needed(&state.everything, &p.manifest.id)
        });
        let Some(p) = unneeded else {
            return Ok(Planned::Steps(state.steps));
        };
        let (id, kind, scope, owner) = (p.manifest.id.clone(), p.kind, p.scope.clone(), p.owner);
        if let Err(question) = state.delete(answers, &id, kind, &scope, owner, false) {
            return Ok(Planned::Ask(question));
        }
    }
}

/// What is installed as a removal being planned leaves it, and the steps
/// so far.
struct State {
    everything: Vec<Installed>,
    records: Records,
    steps: Vec<Step>,
}

impl State {
    /// Plans deleting the package `id` of `kind` in `scope` and the
    /// components it owns, or returns the first question without an
    /// answer.
    fn delete(
        &mut self,
        answers: &BTreeMap<(Owner, String), bool>,
        id: &str,
        kind: Kind,
        scope: &Scope,
        owner: Owner,
        keep_data: bool,
    ) -> Result<(), Question> {
        for (in_scope, component) in self.records.components(owner, id) {
            let Some(c) = self
                .everything
                .iter()
                .find(|p| p.manifest.id == component && p.owner == in_scope)
            else {
                continue;
            };
            // A global component of a local package needs the
            // permission; it stays, without an owner, when that is not
            // granted.
            let local_owner = matches!(owner, Owner::User(_));
            if in_scope == Owner::Global && local_owner {
                match answers.get(&(in_scope, component.clone())) {
                    None => return Err(((in_scope, component), Box::new(c.manifest.clone()))),
                    Some(false) => {
                        let mut record = self.records.get(in_scope, &component);
                        record.owner = None;
                        record.owner_in = None;
                        self.records.set(in_scope, &component, record);
                        self.steps.push(Step::Keep {
                            owner: in_scope,
                            id: component,
                        });
                        continue;
                    }
                    Some(true) => {}
                }
            }
            let (kind, scope) = (c.kind, c.scope.clone());
            self.delete(answers, &component, kind, &scope, in_scope, keep_data)?;
        }
        let had_services = self
            .everything
            .iter()
            .any(|p| p.manifest.id == id && p.owner == owner && !p.manifest.services.is_empty());
        self.everything
            .retain(|p| !(p.manifest.id == id && p.owner == owner));
        self.records.set(owner, id, Default::default());
        self.steps.push(Step::Delete {
            id: id.into(),
            kind,
            scope: scope.clone(),
            owner,
            keep_data,
            had_services,
        });
        Ok(())
    }
}

/// Takes `step`.
fn take(shared: &Shared, root: &Txn<'_>, step: Step) -> Result<(), Status> {
    match step {
        Step::Keep { owner, id } => root.update_records(|records| {
            let mut record = records.get(owner, &id);
            record.owner = None;
            record.owner_in = None;
            records.set(owner, &id, record);
        }),
        Step::Delete {
            id,
            kind,
            scope,
            owner,
            keep_data,
            had_services,
        } => delete(
            shared,
            root,
            &id,
            kind,
            &scope,
            owner,
            keep_data,
            had_services,
        ),
    }
}

/// Deletes the package `id` of `kind` in `scope` and what goes with it,
/// once its components have gone.
#[allow(clippy::too_many_arguments)]
fn delete(
    shared: &Shared,
    root: &Txn<'_>,
    id: &str,
    kind: Kind,
    scope: &Scope,
    owner: Owner,
    keep_data: bool,
    had_services: bool,
) -> Result<(), Status> {
    let subvolumes = shared.subvolumes()?;
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
    root.update_records(|records| records.set(owner, id, Default::default()))
}
