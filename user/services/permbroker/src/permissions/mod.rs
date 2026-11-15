//! The permission broker's records and decisions, apart from the
//! protocol that carries them.
//!
//! A `Store` holds the `system` and `admin` permissions that manifests
//! give each app ID, the grants that users made, and the `admin`
//! permissions that an administrator allowed one process, by its kernel
//! object ID. It answers checks, says when a request needs the prompter
//! or an administrator, records grants, and reports each change that
//! takes a permission away, for the connections that watch it. Grants
//! are kept in memory only.

mod table;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use table::PERMISSIONS;
pub use table::{Scope, scope};

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

/// The app ID of the service manager, which the broker trusts from its
/// start.
pub const SERVICE_MANAGER: &str = "system.servicemgr";
/// The permissions the service manager holds from the start.
pub const SERVICE_MANAGER_PERMISSIONS: &[&str] = &[
    "system.identity.set",
    "system.permissions.read",
    "system.permissions.set",
    "system.devices.manage",
    "admin.power.control",
];
/// The most grants one `ListGrants` reply holds.
pub const LIST_MAX: usize = 256;
/// The permission that `ListGrants` for another user, and `SetGrant` for
/// another user or with `ALLOWED`, need besides the one of their call.
pub const MANAGE_GRANTS: &str = "admin.grants.manage";

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
    /// The permission has the wrong scope for the call.
    InvalidArgs,
}

/// Who a call is for: a user and an app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    pub user: u32,
    pub app_id: &'a str,
    /// The administrator who elevated the client's process, 0 for none.
    pub admin: u32,
    /// The kernel object ID of the client's process, 0 for none.
    pub koid: u64,
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

/// Whether `caller` needs `MANAGE_GRANTS` to list the grants of `user`.
pub fn listing_needs_manage(caller: Identity<'_>, user: u32) -> bool {
    user != caller.user
}

/// Whether `caller` needs `MANAGE_GRANTS` to set `grant`.
pub fn setting_needs_manage(caller: Identity<'_>, grant: &Grant) -> bool {
    grant.user != caller.user || grant.decision == Decision::Allowed
}

impl Call {
    /// The `system` permission a caller needs to make this call.
    pub fn needs(self) -> &'static str {
        match self {
            Call::Check | Call::Request | Call::Watch => "system.permissions.read",
            Call::SetGrant | Call::ListGrants => "system.grants.manage",
            Call::GrantSystem => "system.permissions.set",
            Call::RegisterPrompter => "system.prompts.use",
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
    /// The permission is an `admin` one that the client does not hold:
    /// the session manager must ask an administrator, and an allowed
    /// answer be recorded with `Store::hold`.
    Authorize,
}

#[derive(Default)]
pub struct Store {
    /// The `system` and `admin` permissions of each app ID, for every
    /// user.
    system: BTreeMap<String, BTreeSet<String>>,
    /// The grants, by user, app ID and permission. None is
    /// `Undetermined`.
    grants: BTreeMap<(u32, String, String), Decision>,
    /// The `admin` permissions that an administrator allowed, by the
    /// kernel object ID of the process they were allowed to.
    held: BTreeSet<(u64, String)>,
}

impl Store {
    /// A store with the trusted initial grants: the service manager's,
    /// and `permissions` for the broker's own app ID `own_app_id`, from
    /// its manifest. Fails when `permissions` names a permission that is
    /// unknown or neither a `system` nor an `admin` one.
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

    /// The decision on `permission` for `client`, without prompting, and
    /// whether it allows `permission` only because an administrator
    /// elevated the client: an elevated client holds every `admin` and
    /// `user` permission.
    pub fn check(
        &self,
        client: Identity<'_>,
        permission: &str,
    ) -> Result<(Decision, bool), Failure> {
        let scope = scope(permission).ok_or(Failure::NotFound)?;
        let grant = self.grant(client.user, client.app_id, permission);
        let decision = match scope {
            Scope::System | Scope::Admin if self.has_system(client.app_id, permission) => {
                Decision::Allowed
            }
            Scope::Admin if self.holds(client, permission) => Decision::Allowed,
            Scope::System | Scope::Admin => Decision::Undetermined,
            Scope::User => grant,
            Scope::Default if grant == Decision::Denied => Decision::Denied,
            Scope::Default => Decision::Allowed,
        };
        let elevated = client.admin != 0 && matches!(scope, Scope::Admin | Scope::User);
        Ok(match decision {
            Decision::Allowed => (decision, false),
            _ if elevated => (Decision::Allowed, true),
            _ => (decision, false),
        })
    }

    /// What a request for `permission` by `client` comes to, and whether
    /// it is allowed only because the client is elevated. An `admin`
    /// permission that a client without a process may not have is
    /// `DENIED`.
    pub fn request(
        &self,
        client: Identity<'_>,
        permission: &str,
    ) -> Result<(Requested, bool), Failure> {
        let (decision, elevated) = self.check(client, permission)?;
        let undetermined = decision == Decision::Undetermined;
        Ok(match scope(permission) {
            Some(Scope::User) if undetermined => (Requested::Ask, false),
            Some(Scope::Admin) if undetermined && client.koid != 0 => (Requested::Authorize, false),
            Some(Scope::Admin) if undetermined => (Requested::Decided(Decision::Denied), false),
            _ => (Requested::Decided(decision), elevated),
        })
    }

    /// Whether an administrator allowed `permission` to the process of
    /// `client`.
    fn holds(&self, client: Identity<'_>, permission: &str) -> bool {
        client.koid != 0 && self.held.contains(&(client.koid, permission.into()))
    }

    /// Records that an administrator allowed the `admin` permission
    /// `permission` to the process of `client`, which alone holds it
    /// from then on. Nothing is recorded for a client without a process.
    pub fn hold(&mut self, client: Identity<'_>, permission: &str) {
        if client.koid != 0 && scope(permission) == Some(Scope::Admin) {
            self.held.insert((client.koid, permission.into()));
        }
    }

    /// Fails with `NOT_FOUND` or `INVALID_ARGS` when `set_grant` would
    /// refuse `grant`, without changing anything.
    pub fn check_grant(grant: &Grant) -> Result<(), Failure> {
        let scope = scope(&grant.permission).ok_or(Failure::NotFound)?;
        if matches!(scope, Scope::System | Scope::Admin) || grant.decision == Decision::Undetermined
        {
            return Err(Failure::InvalidArgs);
        }
        Ok(())
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

    /// Sets the grant `grant`, which must be `ALLOWED` or `DENIED` for a
    /// known `user` or `default` permission. Returns what it takes away,
    /// if anything.
    pub fn set_grant(&mut self, grant: &Grant) -> Result<Option<Revoked>, Failure> {
        Store::check_grant(grant)?;
        let scope = scope(&grant.permission).ok_or(Failure::NotFound)?;
        let before = self.put(grant.user, &grant.app_id, &grant.permission, grant.decision);
        let after = grant.decision;
        let taken = (before == Decision::Allowed && after != Decision::Allowed)
            || (scope == Scope::Default && after == Decision::Denied && before != Decision::Denied);
        Ok(taken.then(|| Revoked {
            user: grant.user,
            app_id: grant.app_id.clone(),
            permission: grant.permission.clone(),
        }))
    }

    /// Stores `decision`, `ALLOWED` or `DENIED`, and returns the one it
    /// replaces.
    fn put(&mut self, user: u32, app_id: &str, permission: &str, decision: Decision) -> Decision {
        let key = (user, app_id.into(), permission.into());
        let before = self.grants.insert(key, decision);
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

    /// Adds `permissions` to the `system` and `admin` permissions of
    /// `app_id`. Fails, adding none, when one is unknown or of another
    /// scope.
    pub fn grant_system(&mut self, app_id: &str, permissions: &[String]) -> Result<(), Failure> {
        self.add_system(app_id, permissions)
    }

    fn add_system(&mut self, app_id: &str, permissions: &[String]) -> Result<(), Failure> {
        for p in permissions {
            match scope(p) {
                None => return Err(Failure::NotFound),
                Some(Scope::System | Scope::Admin) => {}
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
