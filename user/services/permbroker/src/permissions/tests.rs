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

/// The kernel object ID of the process of `id`'s identities.
const KOID: u64 = 7;

fn id(app_id: &str) -> Identity<'_> {
    Identity {
        user: 1,
        app_id,
        admin: 0,
        koid: KOID,
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
    assert_eq!(PERMISSIONS.len(), 55);
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

/// The verbs that a permission's name may end with.
const VERBS: [&str; 12] = [
    "access",
    "read",
    "write",
    "use",
    "create",
    "install",
    "manage",
    "control",
    "set",
    "debug",
    "mount",
    "authenticate",
];

#[test]
fn names_are_scope_thing_and_verb() {
    let part = |p: &str| {
        p.starts_with(|c: char| c.is_ascii_lowercase())
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    for (name, scope) in PERMISSIONS {
        let parts: Vec<&str> = name.split('.').collect();
        assert!(name.len() <= 64, "{name}");
        assert!(parts.len() == 3 && parts.iter().all(|p| part(p)), "{name}");
        let named = match parts[0] {
            "system" => Scope::System,
            "admin" => Scope::Admin,
            "user" => Scope::User,
            "default" => Scope::Default,
            other => panic!("{name}: no scope {other}"),
        };
        assert_eq!(named, *scope, "{name}");
        assert!(VERBS.contains(&parts[2]), "{name}");
    }
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
            koid: KOID,
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
        koid: KOID,
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
        koid: KOID,
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

#[test]
fn admin_requests_ask_an_administrator_and_hold_for_one_process() {
    let mut s = store();
    assert_eq!(
        s.request(id(APP), "admin.time.set"),
        Ok((Requested::Authorize, false))
    );
    // Checks never ask.
    assert_eq!(
        s.check(id(APP), "admin.time.set"),
        Ok((Decision::Undetermined, false))
    );
    s.hold(id(APP), "admin.time.set");
    assert_eq!(
        s.check(id(APP), "admin.time.set"),
        Ok((Decision::Allowed, false))
    );
    assert_eq!(
        s.request(id(APP), "admin.time.set"),
        Ok((Requested::Decided(Decision::Allowed), false))
    );
    // Only that permission, only that process, and nothing stored.
    assert_eq!(
        s.request(id(APP), "admin.logs.read"),
        Ok((Requested::Authorize, false))
    );
    let later = Identity {
        koid: KOID + 1,
        ..id(APP)
    };
    assert_eq!(
        s.check(later, "admin.time.set"),
        Ok((Decision::Undetermined, false))
    );
    assert!(s.list(1).is_empty());
    // A manifest's admin permission needs no administrator.
    assert_eq!(
        s.request(id(BROKER), "admin.logs.read"),
        Ok((Requested::Decided(Decision::Allowed), false))
    );
}

#[test]
fn admin_requests_without_a_process_are_denied() {
    let mut s = store();
    let none = Identity { koid: 0, ..id(APP) };
    assert_eq!(
        s.request(none, "admin.time.set"),
        Ok((Requested::Decided(Decision::Denied), false))
    );
    s.hold(none, "admin.time.set");
    assert_eq!(
        s.check(none, "admin.time.set"),
        Ok((Decision::Undetermined, false))
    );
    // Holds are for admin permissions only.
    s.hold(id(APP), "user.camera.use");
    s.hold(id(APP), "system.devices.manage");
    assert_eq!(
        s.check(id(APP), "user.camera.use"),
        Ok((Decision::Undetermined, false))
    );
    assert_eq!(
        s.check(id(APP), "system.devices.manage"),
        Ok((Decision::Undetermined, false))
    );
}

#[test]
fn other_users_and_allowing_need_manage_grants() {
    let me = id(APP);
    assert!(!listing_needs_manage(me, 1));
    assert!(listing_needs_manage(me, 2));
    assert!(!setting_needs_manage(
        me,
        &grant(1, "user.camera.use", Decision::Denied)
    ));
    assert!(setting_needs_manage(
        me,
        &grant(1, "user.camera.use", Decision::Allowed)
    ));
    assert!(setting_needs_manage(
        me,
        &grant(2, "user.camera.use", Decision::Denied)
    ));
    assert_eq!(scope(MANAGE_GRANTS), Some(Scope::Admin));
    assert_eq!(
        Store::check_grant(&grant(1, "user.camera.use", Decision::Allowed)),
        Ok(())
    );
    assert_eq!(
        Store::check_grant(&grant(1, "admin.grants.manage", Decision::Allowed)),
        Err(Failure::InvalidArgs)
    );
    assert_eq!(
        Store::check_grant(&grant(1, "nope", Decision::Denied)),
        Err(Failure::NotFound)
    );
}
