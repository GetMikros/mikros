//! The permission broker's records and decisions, apart from the
//! protocol that carries them.
//!
//! A `Store` holds the system permissions of each app ID and the grants
//! that users made. It answers checks, says when a request needs the
//! prompter, records grants, and reports each change that takes a
//! permission away, for the connections that watch it. Grants are kept
//! in memory only.

#![no_std]

extern crate alloc;

mod table;
#[cfg(test)]
mod tests;

pub use table::{Kind, PERMISSIONS, kind};

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

/// The app ID of the service manager, which the broker trusts from its
/// start.
pub const SERVICE_MANAGER: &str = "system.service-manager";
/// The system permissions the service manager holds from the start.
pub const SERVICE_MANAGER_PERMISSIONS: &[&str] = &[
    "process.set_identity",
    "permissions.check",
    "permissions.grant_system",
];
/// The most grants one `ListGrants` reply holds.
pub const LIST_MAX: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Undetermined,
    Allowed,
    Denied,
}

/// Why a call fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The caller lacks the permission the call needs.
    AccessDenied,
    /// The broker does not know the permission.
    NotFound,
    /// The permission has the wrong kind for the call.
    InvalidArgs,
}

/// Who a call is for: a user and an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    pub user: u32,
    pub app_id: &'a str,
}

/// A user's decision on a `user` or `default` permission for an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub user: u32,
    pub app_id: String,
    pub permission: String,
    pub decision: Decision,
}

/// A permission taken away from an app for a user, which the
/// connections watching that permission are told of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revoked {
    pub user: u32,
    pub app_id: String,
    pub permission: String,
}

/// The calls of the broker protocol, by the permission each needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Check,
    Request,
    Watch,
    SetGrant,
    ListGrants,
    GrantSystem,
    RegisterPrompter,
}

impl Call {
    /// The system permission a caller needs to make this call.
    pub fn needs(self) -> &'static str {
        match self {
            Call::Check | Call::Request | Call::Watch => "permissions.check",
            Call::SetGrant | Call::ListGrants => "permissions.manage",
            Call::GrantSystem => "permissions.grant_system",
            Call::RegisterPrompter => "permissions.prompt",
        }
    }
}

/// What a request comes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requested {
    /// The decision, without asking anyone.
    Decided(Decision),
    /// The permission is a `user` one that the user has not decided:
    /// the prompter must be asked, and its answer recorded with
    /// `Store::record`.
    Ask,
}

#[derive(Default)]
pub struct Store {
    /// The system permissions of each app ID, for every user.
    system: BTreeMap<String, BTreeSet<String>>,
    /// The grants, by user, app ID and permission. None is
    /// `Undetermined`.
    grants: BTreeMap<(u32, String, String), Decision>,
}

impl Store {
    /// A store with the trusted initial grants: the service manager's,
    /// and `permissions` for the broker's own app ID `own_app_id`, from
    /// its manifest. Fails when `permissions` names a permission that is
    /// unknown or not a `system` one.
    pub fn bootstrap(own_app_id: &str, permissions: &[String]) -> Result<Store, Failure> {
        let mut store = Store::default();
        let manager = SERVICE_MANAGER_PERMISSIONS.iter().map(|p| String::from(*p));
        store
            .system
            .insert(SERVICE_MANAGER.into(), manager.collect());
        store.add_system(own_app_id, permissions)?;
        Ok(store)
    }

    /// Fails with `AccessDenied` unless `caller` may make `call`.
    pub fn authorize(&self, caller: &str, call: Call) -> Result<(), Failure> {
        if self.has_system(caller, call.needs()) {
            Ok(())
        } else {
            Err(Failure::AccessDenied)
        }
    }

    fn has_system(&self, app_id: &str, permission: &str) -> bool {
        self.system
            .get(app_id)
            .is_some_and(|set| set.contains(permission))
    }

    fn grant(&self, user: u32, app_id: &str, permission: &str) -> Decision {
        self.grants
            .get(&(user, app_id.into(), permission.into()))
            .copied()
            .unwrap_or(Decision::Undetermined)
    }

    /// The decision on `permission` for `client`, without prompting.
    pub fn check(&self, client: Identity<'_>, permission: &str) -> Result<Decision, Failure> {
        let kind = kind(permission).ok_or(Failure::NotFound)?;
        let grant = self.grant(client.user, client.app_id, permission);
        Ok(match kind {
            Kind::System if self.has_system(client.app_id, permission) => Decision::Allowed,
            Kind::System => Decision::Undetermined,
            Kind::User => grant,
            Kind::Default if grant == Decision::Denied => Decision::Denied,
            Kind::Default => Decision::Allowed,
        })
    }

    /// What a request for `permission` by `client` comes to.
    pub fn request(&self, client: Identity<'_>, permission: &str) -> Result<Requested, Failure> {
        let decision = self.check(client, permission)?;
        Ok(match kind(permission) {
            Some(Kind::User) if decision == Decision::Undetermined => Requested::Ask,
            _ => Requested::Decided(decision),
        })
    }

    /// Records the prompter's answer to a request, and returns the
    /// decision it makes.
    pub fn record(&mut self, client: Identity<'_>, permission: &str, allowed: bool) -> Decision {
        let decision = if allowed {
            Decision::Allowed
        } else {
            Decision::Denied
        };
        // A prompt is only asked for an undetermined permission, so
        // recording its answer takes nothing away.
        self.put(client.user, client.app_id, permission, decision);
        decision
    }

    /// Sets the grant `grant`, where `Undetermined` removes it. Returns
    /// what it takes away, if anything.
    pub fn set_grant(&mut self, grant: &Grant) -> Result<Option<Revoked>, Failure> {
        let kind = kind(&grant.permission).ok_or(Failure::NotFound)?;
        if kind == Kind::System {
            return Err(Failure::InvalidArgs);
        }
        let before = self.put(grant.user, &grant.app_id, &grant.permission, grant.decision);
        let after = grant.decision;
        let taken = (before == Decision::Allowed && after != Decision::Allowed)
            || (kind == Kind::Default && after == Decision::Denied && before != Decision::Denied);
        Ok(taken.then(|| Revoked {
            user: grant.user,
            app_id: grant.app_id.clone(),
            permission: grant.permission.clone(),
        }))
    }

    /// Stores `decision` and returns the one it replaces.
    fn put(&mut self, user: u32, app_id: &str, permission: &str, decision: Decision) -> Decision {
        let key = (user, app_id.into(), permission.into());
        let before = if decision == Decision::Undetermined {
            self.grants.remove(&key)
        } else {
            self.grants.insert(key, decision)
        };
        before.unwrap_or(Decision::Undetermined)
    }

    /// The grants of `user`, at most `LIST_MAX` of them, by app ID and
    /// permission.
    pub fn list(&self, user: u32) -> Vec<Grant> {
        self.grants
            .range((user, String::new(), String::new())..)
            .take_while(|((u, _, _), _)| *u == user)
            .take(LIST_MAX)
            .map(|((user, app_id, permission), decision)| Grant {
                user: *user,
                app_id: app_id.clone(),
                permission: permission.clone(),
                decision: *decision,
            })
            .collect()
    }

    /// Adds `permissions` to the system permissions of `app_id`. Fails,
    /// adding none, when one is unknown or not a `system` permission.
    pub fn grant_system(&mut self, app_id: &str, permissions: &[String]) -> Result<(), Failure> {
        self.add_system(app_id, permissions)
    }

    fn add_system(&mut self, app_id: &str, permissions: &[String]) -> Result<(), Failure> {
        for p in permissions {
            match kind(p) {
                None => return Err(Failure::NotFound),
                Some(Kind::System) => {}
                Some(_) => return Err(Failure::InvalidArgs),
            }
        }
        self.system
            .entry(app_id.into())
            .or_default()
            .extend(permissions.iter().cloned());
        Ok(())
    }
}
