extern crate std;

use crate::permissions::*;
use alloc::string::ToString;
use alloc::vec;

const BROKER: &str = "system.permbroker";
const APP: &str = "org.example.app";

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn store() -> Store {
    Store::bootstrap(BROKER, &strings(&["admin.logs.read"])).unwrap()
}

fn id(app_id: &str) -> Identity<'_> {
    Identity {
        user: 1,
        app_id,
        admin: 0,
    }
}

fn grant(user: u32, permission: &str, decision: Decision) -> Grant {
    Grant {
        user,
        app_id: APP.to_string(),
        permission: permission.to_string(),
        decision,
    }
}

#[test]
fn table_is_sorted_and_unique() {
    assert!(PERMISSIONS.windows(2).all(|w| w[0].0 < w[1].0));
    assert_eq!(PERMISSIONS.len(), 56);
    assert_eq!(scope("user.camera.use"), Some(Scope::User));
    assert_eq!(scope("user.developer.access"), Some(Scope::User));
    assert_eq!(scope("system.diagnostics.read"), Some(Scope::System));
    assert_eq!(scope("default.network.use"), Some(Scope::Default));
    assert_eq!(scope("system.permissions.read"), Some(Scope::System));
    assert_eq!(scope("system.gpu_priority.use"), Some(Scope::System));
    assert_eq!(scope("system.boot_state.set"), Some(Scope::System));
    assert_eq!(scope("system.log_sources.create"), Some(Scope::System));
    assert_eq!(scope("admin.services.control"), Some(Scope::Admin));
    assert_eq!(scope("admin.grants.manage"), Some(Scope::Admin));
    assert_eq!(scope("nope"), None);
}

#[test]
fn admin_permissions_are_these() {
    let admin: Vec<&str> = PERMISSIONS
        .iter()
        .filter(|(_, scope)| *scope == Scope::Admin)
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        admin,
        [
            "admin.accounts.manage",
            "admin.grants.manage",
            "admin.logs.read",
            "admin.power.control",
            "admin.services.control",
            "admin.time.set",
        ]
    );
}

#[test]
fn bootstrap_trusts_the_service_manager_and_the_broker_manifest() {
    let s = store();
    for p in SERVICE_MANAGER_PERMISSIONS {
        assert_eq!(
            s.check(id(SERVICE_MANAGER), p).map(|d| d.0),
            Ok(Decision::Allowed)
        );
    }
    assert_eq!(s.authorize(SERVICE_MANAGER, Call::Check), Ok(()));
    assert_eq!(s.authorize(SERVICE_MANAGER, Call::GrantSystem), Ok(()));
    assert_eq!(
        s.authorize(SERVICE_MANAGER, Call::SetGrant),
        Err(Failure::AccessDenied)
    );
    assert_eq!(
        s.check(id(BROKER), "admin.logs.read").map(|d| d.0),
        Ok(Decision::Allowed)
    );
}

#[test]
fn bootstrap_refuses_a_bad_manifest() {
    assert_eq!(
        Store::bootstrap(BROKER, &strings(&["user.camera.use"])).err(),
        Some(Failure::InvalidArgs)
    );
    assert_eq!(
        Store::bootstrap(BROKER, &strings(&["nope"])).err(),
        Some(Failure::NotFound)
    );
}

#[test]
fn each_call_needs_its_permission() {
    let calls = [
        (Call::Check, "system.permissions.read"),
        (Call::Request, "system.permissions.read"),
        (Call::Watch, "system.permissions.read"),
        (Call::SetGrant, "system.grants.manage"),
        (Call::ListGrants, "system.grants.manage"),
        (Call::GrantSystem, "system.permissions.set"),
        (Call::RegisterPrompter, "system.prompts.use"),
    ];
    for (call, needs) in calls {
        let mut s = store();
        assert_eq!(s.authorize(APP, call), Err(Failure::AccessDenied));
        s.grant_system(APP, &strings(&[needs])).unwrap();
        assert_eq!(s.authorize(APP, call), Ok(()));
    }
}

#[test]
fn system_permissions_apply_for_every_user() {
    let mut s = store();
    assert_eq!(
        s.check(id(APP), "system.devices.manage").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    s.grant_system(APP, &strings(&["system.devices.manage"]))
        .unwrap();
    for user in [0, 1, 1000] {
        let who = Identity {
            user,
            app_id: APP,
            admin: 0,
        };
        assert_eq!(
            s.check(who, "system.devices.manage").map(|d| d.0),
            Ok(Decision::Allowed)
        );
    }
}

#[test]
fn grant_system_adds_and_checks_every_name_first() {
    let mut s = store();
    s.grant_system(APP, &strings(&["admin.logs.read"])).unwrap();
    s.grant_system(APP, &strings(&["admin.time.set"])).unwrap();
    assert_eq!(
        s.check(id(APP), "admin.logs.read").map(|d| d.0),
        Ok(Decision::Allowed)
    );
    assert_eq!(
        s.check(id(APP), "admin.time.set").map(|d| d.0),
        Ok(Decision::Allowed)
    );
    s.grant_system(APP, &strings(&["system.devices.manage"]))
        .unwrap();
    let bad = strings(&["admin.power.control", "user.camera.use"]);
    assert_eq!(s.grant_system(APP, &bad), Err(Failure::InvalidArgs));
    let unknown = strings(&["admin.power.control", "nope"]);
    assert_eq!(s.grant_system(APP, &unknown), Err(Failure::NotFound));
    assert_eq!(
        s.check(id(APP), "admin.power.control").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
}

#[test]
fn checks_by_scope() {
    let mut s = store();
    assert_eq!(
        s.check(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    assert_eq!(
        s.check(id(APP), "default.network.use").map(|d| d.0),
        Ok(Decision::Allowed)
    );
    assert_eq!(
        s.check(id(APP), "nope").map(|d| d.0),
        Err(Failure::NotFound)
    );
    s.set_grant(&grant(1, "user.camera.use", Decision::Allowed))
        .unwrap();
    s.set_grant(&grant(1, "default.network.use", Decision::Denied))
        .unwrap();
    assert_eq!(
        s.check(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Decision::Allowed)
    );
    assert_eq!(
        s.check(id(APP), "default.network.use").map(|d| d.0),
        Ok(Decision::Denied)
    );
    // Another user's grants are their own.
    let other = Identity {
        user: 2,
        app_id: APP,
        admin: 0,
    };
    assert_eq!(
        s.check(other, "user.camera.use").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    assert_eq!(
        s.check(other, "default.network.use").map(|d| d.0),
        Ok(Decision::Allowed)
    );
    s.set_grant(&grant(1, "user.camera.use", Decision::Denied))
        .unwrap();
    assert_eq!(
        s.check(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Decision::Denied)
    );
}

#[test]
fn requests_ask_only_for_undetermined_user_permissions() {
    let mut s = store();
    assert_eq!(
        s.request(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Requested::Ask)
    );
    assert_eq!(
        s.request(id(APP), "default.network.use").map(|d| d.0),
        Ok(Requested::Decided(Decision::Allowed))
    );
    assert_eq!(
        s.request(id(APP), "system.devices.manage").map(|d| d.0),
        Ok(Requested::Decided(Decision::Undetermined))
    );
    assert_eq!(
        s.request(id(APP), "nope").map(|d| d.0),
        Err(Failure::NotFound)
    );
    assert_eq!(
        s.record(id(APP), "user.camera.use", false),
        Decision::Denied
    );
    assert_eq!(
        s.request(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Requested::Decided(Decision::Denied))
    );
    assert_eq!(
        s.record(id(APP), "user.microphone.use", true),
        Decision::Allowed
    );
    assert_eq!(
        s.request(id(APP), "user.microphone.use").map(|d| d.0),
        Ok(Requested::Decided(Decision::Allowed))
    );
}

#[test]
fn set_grant_refuses_system_admin_and_unknown_permissions() {
    let mut s = store();
    for permission in ["system.devices.manage", "admin.time.set"] {
        for decision in [Decision::Allowed, Decision::Denied] {
            assert_eq!(
                s.set_grant(&grant(1, permission, decision)),
                Err(Failure::InvalidArgs)
            );
        }
    }
    assert!(s.list(1).is_empty());
    assert_eq!(
        s.set_grant(&grant(1, "nope", Decision::Allowed)),
        Err(Failure::NotFound)
    );
}

#[test]
fn revocations() {
    let mut s = store();
    let revoked = |p: &str| {
        Some(Revoked {
            user: 1,
            app_id: APP.to_string(),
            permission: p.to_string(),
        })
    };
    // User permissions: only a change away from ALLOWED.
    assert_eq!(
        s.set_grant(&grant(1, "user.camera.use", Decision::Denied)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "user.camera.use", Decision::Allowed)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "user.camera.use", Decision::Denied)),
        Ok(revoked("user.camera.use"))
    );
    // Default permissions: a new DENIED grant too.
    assert_eq!(
        s.set_grant(&grant(1, "default.network.use", Decision::Denied)),
        Ok(revoked("default.network.use"))
    );
    assert_eq!(
        s.set_grant(&grant(1, "default.network.use", Decision::Denied)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "default.network.use", Decision::Allowed)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "default.network.use", Decision::Denied)),
        Ok(revoked("default.network.use"))
    );
}

#[test]
fn set_grant_refuses_undetermined() {
    let mut s = store();
    s.set_grant(&grant(1, "user.camera.use", Decision::Denied))
        .unwrap();
    assert_eq!(
        s.set_grant(&grant(1, "user.camera.use", Decision::Undetermined)),
        Err(Failure::InvalidArgs)
    );
    assert_eq!(
        s.check(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Decision::Denied)
    );
}

#[test]
fn list_holds_one_users_grants_in_order() {
    let mut s = store();
    s.set_grant(&grant(1, "user.microphone.use", Decision::Allowed))
        .unwrap();
    s.set_grant(&grant(1, "user.camera.use", Decision::Denied))
        .unwrap();
    s.set_grant(&grant(2, "user.camera.use", Decision::Allowed))
        .unwrap();
    s.set_grant(&grant(0, "user.camera.use", Decision::Allowed))
        .unwrap();
    assert_eq!(
        s.list(1),
        vec![
            grant(1, "user.camera.use", Decision::Denied),
            grant(1, "user.microphone.use", Decision::Allowed),
        ]
    );
    assert!(s.list(3).is_empty());
}

#[test]
fn list_stops_at_its_limit() {
    let mut s = store();
    for i in 0..LIST_MAX + 10 {
        let mut g = grant(1, "user.camera.use", Decision::Allowed);
        g.app_id = std::format!("app.{i:04}");
        s.set_grant(&g).unwrap();
    }
    assert_eq!(s.list(1).len(), LIST_MAX);
}

#[test]
fn elevated_clients_hold_user_and_admin_permissions() {
    let mut s = store();
    let elevated = Identity {
        user: 1,
        app_id: APP,
        admin: 1000,
    };
    // Every user and admin permission, without asking or recording a
    // grant.
    assert_eq!(
        s.check(elevated, "user.camera.use"),
        Ok((Decision::Allowed, true))
    );
    assert_eq!(
        s.check(elevated, "user.volumes.access"),
        Ok((Decision::Allowed, true))
    );
    assert_eq!(
        s.check(elevated, "admin.power.control"),
        Ok((Decision::Allowed, true))
    );
    assert_eq!(
        s.check(elevated, "admin.logs.read"),
        Ok((Decision::Allowed, true))
    );
    for (permission, _) in PERMISSIONS
        .iter()
        .filter(|(_, scope)| matches!(scope, Scope::User | Scope::Admin))
    {
        assert_eq!(s.check(elevated, permission), Ok((Decision::Allowed, true)));
        assert_eq!(
            s.request(elevated, permission),
            Ok((Requested::Decided(Decision::Allowed), true))
        );
    }
    assert_eq!(
        s.request(elevated, "user.camera.use"),
        Ok((Requested::Decided(Decision::Allowed), true))
    );
    assert_eq!(
        s.check(id(APP), "user.camera.use").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    assert!(s.list(1).is_empty());
    // Full disk access too, but never a system permission.
    assert_eq!(
        s.check(elevated, "user.full_disk.access"),
        Ok((Decision::Allowed, true))
    );
    assert_eq!(
        s.check(elevated, "system.devices.manage").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    assert_eq!(
        s.check(elevated, "system.identity.set").map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    // A default permission a user denied stays denied.
    s.set_grant(&grant(1, "default.network.use", Decision::Denied))
        .unwrap();
    assert_eq!(
        s.check(elevated, "default.network.use"),
        Ok((Decision::Denied, false))
    );
    // What is allowed anyway is not counted as elevation.
    s.set_grant(&grant(1, "user.camera.use", Decision::Allowed))
        .unwrap();
    assert_eq!(
        s.check(elevated, "user.camera.use"),
        Ok((Decision::Allowed, false))
    );
}
