//! Drives the permission broker's store with a sequence of operations,
//! each from a few bytes: SetGrant, Check, Request, the prompter's
//! answer (`record`), an administrator's (`hold`) and system grants. A
//! reference model of the rules of MIKSPEC-0037 runs beside it, and every
//! answer, decision and revocation must be the model's.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::{BTreeMap, BTreeSet};
use user_fuzz::permissions::{
    BUILD, Decision, Failure, Grant, Identity, Requested, Scope, Store, scope,
};

/// Permissions of every scope, and one that the broker does not know.
const PERMISSIONS: [&str; 9] = [
    "system.permissions.read",
    "system.devices.manage",
    "admin.time.set",
    "admin.grants.manage",
    "user.camera.use",
    "user.location.read",
    "default.network.use",
    "default.gpu.use",
    "test.unknown.use",
];

const APPS: [&str; 3] = ["test.one", "test.two", "test.three"];
const CHECKSUMS: [&str; 3] = ["", BUILD, "sha256:00"];
const KOIDS: [u64; 3] = [0, 5, 6];

/// The rules of the store, as plainly as they can be written.
#[derive(Default)]
struct Model {
    system: BTreeSet<(String, String)>,
    grants: BTreeMap<(u32, String, String, String), Decision>,
    held: BTreeSet<(u64, String)>,
}

impl Model {
    fn holds(&self, client: &Identity<'_>, permission: &str) -> bool {
        client.koid != 0 && self.held.contains(&(client.koid, permission.to_owned()))
    }

    fn check(&self, client: &Identity<'_>, permission: &str) -> Result<(Decision, bool), Failure> {
        let scope = scope(permission).ok_or(Failure::NotFound)?;
        let build = client.checksum == BUILD;
        let grant = if build && self.holds(client, permission) {
            Decision::Allowed
        } else if build {
            Decision::Undetermined
        } else {
            let key = (
                client.user,
                client.app_id.to_owned(),
                client.checksum.to_owned(),
                permission.to_owned(),
            );
            self.grants
                .get(&key)
                .copied()
                .unwrap_or(Decision::Undetermined)
        };
        let system = client.checksum.is_empty()
            && self
                .system
                .contains(&(client.app_id.to_owned(), permission.to_owned()));
        let decision = match scope {
            Scope::System | Scope::Admin if system => Decision::Allowed,
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

    fn request(
        &self,
        client: &Identity<'_>,
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
}

fuzz_target!(|data: &[u8]| {
    let mut store = Store::default();
    let mut model = Model::default();
    for op in data.chunks_exact(4) {
        let [kind, who, what, how] = [op[0], op[1], op[2], op[3]];
        let permission = PERMISSIONS[usize::from(what) % PERMISSIONS.len()];
        let app_id = APPS[usize::from(who) % APPS.len()];
        let client = Identity {
            user: u32::from(who >> 2 & 1) + 1,
            app_id,
            admin: u32::from(who >> 3 & 1) * 1000,
            koid: KOIDS[usize::from(who >> 4) % KOIDS.len()],
            checksum: CHECKSUMS[usize::from(who >> 6) % CHECKSUMS.len()],
        };
        let decision = match how % 3 {
            0 => Decision::Undetermined,
            1 => Decision::Allowed,
            _ => Decision::Denied,
        };
        match kind % 6 {
            0 => assert_eq!(
                store.check(client, permission),
                model.check(&client, permission),
                "Check {permission} for {client:?}"
            ),
            1 => assert_eq!(
                store.request(client, permission),
                model.request(&client, permission),
                "Request {permission} for {client:?}"
            ),
            2 => {
                let grant = Grant {
                    user: client.user,
                    app_id: app_id.into(),
                    permission: permission.into(),
                    decision,
                };
                let expected = match (scope(permission), decision) {
                    (None, _) => Err(Failure::NotFound),
                    (Some(Scope::System | Scope::Admin), _) | (_, Decision::Undetermined) => {
                        Err(Failure::InvalidArgs)
                    }
                    (Some(scope), _) => {
                        let key = (client.user, app_id.into(), String::new(), permission.into());
                        let before = model
                            .grants
                            .insert(key, decision)
                            .unwrap_or(Decision::Undetermined);
                        // A grant that takes the permission away is a
                        // revocation, and only such a grant.
                        let taken = (before == Decision::Allowed && decision != Decision::Allowed)
                            || (scope == Scope::Default
                                && decision == Decision::Denied
                                && before != Decision::Denied);
                        Ok(taken)
                    }
                };
                let set = store.set_grant(&grant).map(|revoked| revoked.is_some());
                assert_eq!(set, expected, "SetGrant {grant:?}");
            }
            3 => {
                // The prompter's answer to an `Ask`.
                let allowed = how & 1 == 1;
                let answer = if allowed {
                    Decision::Allowed
                } else {
                    Decision::Denied
                };
                if client.checksum == BUILD {
                    if allowed && client.koid != 0 {
                        model.held.insert((client.koid, permission.into()));
                    }
                } else {
                    let key = (
                        client.user,
                        app_id.into(),
                        client.checksum.into(),
                        permission.into(),
                    );
                    model.grants.insert(key, answer);
                }
                assert_eq!(store.record(client, permission, allowed), answer);
            }
            4 => {
                // An administrator's yes to an `Authorize`.
                if client.koid != 0 && scope(permission) == Some(Scope::Admin) {
                    model.held.insert((client.koid, permission.into()));
                }
                store.hold(client, permission);
            }
            _ => {
                let all = [permission.to_owned()];
                let expected = match scope(permission) {
                    None => Err(Failure::NotFound),
                    Some(Scope::System | Scope::Admin) => {
                        model.system.insert((app_id.into(), permission.into()));
                        Ok(())
                    }
                    Some(_) => Err(Failure::InvalidArgs),
                };
                assert_eq!(store.grant_system(app_id, &all), expected, "GrantSystem");
            }
        }
    }
});
