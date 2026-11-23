//! The permission broker's records and decisions, apart from the
//! protocol that carries them.
//!
//! A `Store` holds the `system` and `admin` permissions that manifests
//! give each app ID, the grants that users made, and the `admin`
//! permissions that an administrator allowed one process, by its kernel
//! object ID. It answers checks, says when a request needs the prompter
//! or an administrator, records grants, and reports each change that
//! takes a permission away, for the connections that watch it. It
//! writes the grants and the numbers of app IDs as text, one per line,
//! and reads them back after a restart; the `system` and `admin`
//! permissions are not written.

mod asks;
mod table;
#[cfg(test)]
mod tests;

// Only the service, which runs on Mikros alone, asks.
#[cfg_attr(not(target_os = "mikros"), allow(unused_imports))]
pub use asks::{Asks, Begin, Grant as AskGrant};
#[cfg(test)]
pub use table::PERMISSIONS;
pub use table::{Scope, scope};
// Only the service, which runs on Mikros alone, lists the names.
#[cfg_attr(not(target_os = "mikros"), allow(unused_imports))]
pub use table::names_after;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

/// What a question for a client names as the service that asks for it:
/// the caller's app ID when its kernel object ID is not the client's,
/// except for the process manager, which asks only for the processes it
/// starts, and empty otherwise (MIKSPEC-0037.R54).
pub fn via(caller_app: &str, caller_koid: u64, client_koid: u64) -> String {
    match caller_koid == client_koid || caller_app == PROCESS_MANAGER {
        true => String::new(),
        false => caller_app.into(),
    }
}

/// The app ID of the service manager, which the broker trusts from its
/// start.
pub const SERVICE_MANAGER: &str = "system.servicemgr";
/// The app ID of the process manager, whose questions name no service.
pub const PROCESS_MANAGER: &str = "system.processmgr";
/// The permissions the service manager holds from the start.
pub const SERVICE_MANAGER_PERMISSIONS: &[&str] = &[
    "system.identity.set",
    "system.permissions.read",
    "system.permissions.request",
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
    /// The checksum of the portable app's package, `build` for a build,
    /// and empty otherwise (MIKSPEC-0036.R41).
    pub checksum: &'a str,
}

/// The `portable_checksum` of a build, whose grants are never kept
/// (MIKSPEC-0037.R35).
pub const BUILD: &str = "build";

impl Identity<'_> {
    fn build(&self) -> bool {
        self.checksum == BUILD
    }
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
            Call::Check | Call::Watch => "system.permissions.read",
            Call::Request => "system.permissions.request",
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
    /// The grants, by user, app ID, the checksum of a portable app's
    /// package or nothing for an installed app (MIKSPEC-0101.R46), and
    /// permission. None is `Undetermined`.
    grants: BTreeMap<(u32, String, String, String), Decision>,
    /// The `admin` permissions that an administrator allowed, by the
    /// kernel object ID of the process they were allowed to.
    held: BTreeSet<(u64, String)>,
    /// The number of each app ID that was asked for, and the app ID of
    /// each number.
    numbers: BTreeMap<String, u32>,
    app_ids: BTreeMap<u32, String>,
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

    /// The number of `app_id`: the lowest from 1 that no app ID has, given
    /// the first time it is asked for, and the same from then on; and
    /// whether it is new.
    pub fn number(&mut self, app_id: &str) -> (u32, bool) {
        if let Some(&number) = self.numbers.get(app_id) {
            return (number, false);
        }
        let number = (1..)
            .find(|n| !self.app_ids.contains_key(n))
            .expect("fewer than u32::MAX app IDs");
        self.numbers.insert(app_id.into(), number);
        self.app_ids.insert(number, app_id.into());
        (number, true)
    }

    /// The numbers of the app IDs, a line `<number> <app ID>` each.
    pub fn encode_numbers(&self) -> Vec<u8> {
        let mut text = String::new();
        for (number, app_id) in &self.app_ids {
            if storable(app_id) {
                text.push_str(&alloc::format!("{number} {app_id}\n"));
            }
        }
        text.into_bytes()
    }

    /// Adds the numbers of `bytes`, as `encode_numbers` writes them,
    /// except those of a number or app ID that has one already. Returns
    /// how many lines it skipped.
    pub fn load_numbers(&mut self, bytes: &[u8]) -> usize {
        let mut skipped = 0;
        for line in lines(bytes) {
            let parsed = line.split_once(' ').and_then(|(number, app_id)| {
                let number: u32 = number.parse().ok()?;
                (number != 0 && storable(app_id)).then_some((number, app_id))
            });
            match parsed {
                Some((number, app_id))
                    if !self.app_ids.contains_key(&number)
                        && !self.numbers.contains_key(app_id) =>
                {
                    self.numbers.insert(app_id.into(), number);
                    self.app_ids.insert(number, app_id.into());
                }
                _ => skipped += 1,
            }
        }
        skipped
    }

    /// The `user` and `default` grants, a line
    /// `<user> <app ID> <permission> allowed|denied` each, followed by
    /// ` <checksum>` for a portable app's.
    pub fn encode_grants(&self) -> Vec<u8> {
        let mut text = String::new();
        for ((user, app_id, checksum, permission), decision) in &self.grants {
            let decision = match decision {
                Decision::Allowed => "allowed",
                Decision::Denied => "denied",
                Decision::Undetermined => continue,
            };
            if !storable(app_id) || (!checksum.is_empty() && !storable(checksum)) {
                continue;
            }
            text.push_str(&alloc::format!("{user} {app_id} {permission} {decision}"));
            if !checksum.is_empty() {
                text.push_str(&alloc::format!(" {checksum}"));
            }
            text.push('\n');
        }
        text.into_bytes()
    }

    /// Adds the grants of `bytes`, as `encode_grants` writes them, except
    /// those that `set_grant` would refuse and those of a user, app ID
    /// and permission that have one already. Returns how many lines it
    /// skipped.
    pub fn load_grants(&mut self, bytes: &[u8]) -> usize {
        let mut skipped = 0;
        for line in lines(bytes) {
            let mut words = line.split(' ');
            let words = (
                words.next(),
                words.next(),
                words.next(),
                words.next(),
                words.next(),
                words.next(),
            );
            let checksum = words.4.unwrap_or("");
            let parsed = match words {
                (Some(user), Some(app_id), Some(permission), Some(decision), _, None)
                    if checksum != BUILD =>
                {
                    let decision = match decision {
                        "allowed" => Some(Decision::Allowed),
                        "denied" => Some(Decision::Denied),
                        _ => None,
                    };
                    user.parse()
                        .ok()
                        .zip(decision)
                        .map(|(user, decision)| Grant {
                            user,
                            app_id: app_id.into(),
                            permission: permission.into(),
                            decision,
                        })
                }
                _ => None,
            };
            let key = parsed.as_ref().map(|g| {
                (
                    g.user,
                    g.app_id.clone(),
                    String::from(checksum),
                    g.permission.clone(),
                )
            });
            match (parsed, key) {
                (Some(grant), Some(key))
                    if Store::check_grant(&grant).is_ok() && !self.grants.contains_key(&key) =>
                {
                    self.grants.insert(key, grant.decision);
                }
                _ => skipped += 1,
            }
        }
        skipped
    }

    /// The app ID that has `number`, if one has.
    pub fn app_id(&self, number: u32) -> Option<&str> {
        self.app_ids.get(&number).map(String::as_str)
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

    /// The kept grant of `permission` for `client`; none for a build.
    fn grant(&self, client: Identity<'_>, permission: &str) -> Decision {
        if client.build() {
            return Decision::Undetermined;
        }
        let key = (
            client.user,
            client.app_id.into(),
            client.checksum.into(),
            permission.into(),
        );
        self.grants
            .get(&key)
            .copied()
            .unwrap_or(Decision::Undetermined)
    }

    /// Whether `client` has the `system` or `admin` permission
    /// `permission` of its app ID, which a portable app or a build never
    /// has.
    fn system_of(&self, client: Identity<'_>, permission: &str) -> bool {
        client.checksum.is_empty() && self.has_system(client.app_id, permission)
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
        // A build holds what it was allowed only while its process runs.
        let grant = match client.build() && self.holds(client, permission) {
            true => Decision::Allowed,
            false => self.grant(client, permission),
        };
        let decision = match scope {
            Scope::System | Scope::Admin if self.system_of(client, permission) => Decision::Allowed,
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
    /// decision it makes. A build's is kept only as held by its process
    /// when it allows, and not at all when it denies (MIKSPEC-0037.R35).
    pub fn record(&mut self, client: Identity<'_>, permission: &str, allowed: bool) -> Decision {
        let decision = if allowed {
            Decision::Allowed
        } else {
            Decision::Denied
        };
        if client.build() {
            if allowed && client.koid != 0 {
                self.held.insert((client.koid, permission.into()));
            }
            return decision;
        }
        // A prompt is only asked for an undetermined permission, so
        // recording its answer takes nothing away.
        let key = (
            client.user,
            client.app_id.into(),
            client.checksum.into(),
            permission.into(),
        );
        self.grants.insert(key, decision);
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
    /// The installed app's, whose grants `SetGrant` sets.
    fn put(&mut self, user: u32, app_id: &str, permission: &str, decision: Decision) -> Decision {
        let key = (user, app_id.into(), String::new(), permission.into());
        let before = self.grants.insert(key, decision);
        before.unwrap_or(Decision::Undetermined)
    }

    /// The grants of `user` for installed apps, at most `LIST_MAX` of
    /// them, by app ID and permission.
    pub fn list(&self, user: u32) -> Vec<Grant> {
        self.grants
            .range((user, String::new(), String::new(), String::new())..)
            .take_while(|((u, _, _, _), _)| *u == user)
            .filter(|((_, _, checksum, _), _)| checksum.is_empty())
            .take(LIST_MAX)
            .map(|((user, app_id, _, permission), decision)| Grant {
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

/// Whether `app_id` can be written as a word of a line: it is not empty
/// and holds no space or control character.
fn storable(app_id: &str) -> bool {
    !app_id.is_empty() && !app_id.chars().any(|c| c == ' ' || c.is_control())
}

/// The lines of the text `bytes` that are not empty; none when `bytes`
/// is not UTF-8.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &str> {
    core::str::from_utf8(bytes)
        .unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty())
}
