extern crate std;

use crate::permissions::*;
use alloc::string::ToString;
use alloc::vec;

const BROKER: &str = "system.permbroker";
const APP: &str = "org.example.app";
const USER_PERMISSION: &str = "user.camera.use";
const DEFAULT_PERMISSION: &str = "default.network.use";

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
        checksum: "",
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
fn lists_names_page_by_page() {
    let first: Vec<&str> = names_after("", 2).collect();
    assert_eq!(first, [PERMISSIONS[0].0, PERMISSIONS[1].0]);
    let next: Vec<&str> = names_after(first[1], 1).collect();
    assert_eq!(next, [PERMISSIONS[2].0]);
    // A name between two known ones starts after the earlier one.
    let between = format!("{}a", PERMISSIONS[0].0);
    assert_eq!(names_after(&between, 1).next(), Some(PERMISSIONS[1].0));
    assert_eq!(names_after("", usize::MAX).count(), PERMISSIONS.len());
    assert_eq!(names_after("~", 128).count(), 0);
}

#[test]
fn table_is_sorted_and_unique() {
    assert!(PERMISSIONS.windows(2).all(|w| w[0].0 < w[1].0));
    assert_eq!(PERMISSIONS.len(), 65);
    assert_eq!(scope("user.layer_surfaces.use"), Some(Scope::User));
    assert_eq!(scope("user.shares.use"), Some(Scope::User));
    assert_eq!(scope("admin.devices.read"), Some(Scope::Admin));
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
const VERBS: [&str; 13] = [
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
    "request",
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
            "admin.acls.manage",
            "admin.apps.write",
            "admin.devices.read",
            "admin.grants.manage",
            "admin.logs.read",
            "admin.packages.install",
            "admin.power.control",
            "admin.public.write",
            "admin.services.control",
            "admin.time.set",
            "admin.volumes.mount",
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
        (Call::Request, "system.permissions.request"),
        (Call::Watch, "system.permissions.read"),
        (Call::SetGrant, "system.grants.manage"),
        (Call::ListGrants, "system.grants.manage"),
        (Call::ListSystemGrants, "system.grants.manage"),
        (Call::GrantSystem, "system.permissions.set"),
        (Call::GrantList, "system.permissions.set"),
        (Call::RegistrationsDone, "system.permissions.set"),
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
            checksum: "",
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
    let bad = strings(&["admin.power.control", DEFAULT_PERMISSION]);
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
        checksum: "",
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
        checksum: "",
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

#[test]
fn numbers_app_ids_from_1_and_keeps_their_numbers() {
    let mut s = store();
    assert_eq!(s.number("system.a"), (1, true));
    assert_eq!(s.number("system.b"), (2, true));
    assert_eq!(s.number("system.a"), (1, false));
    assert_eq!(s.app_id(2), Some("system.b"));
    assert_eq!(s.app_id(0), None);
    assert_eq!(s.app_id(3), None);
}

#[test]
fn numbers_survive_a_restart() {
    let mut s = store();
    s.number("system.a");
    s.number("org.example.b");
    let bytes = s.encode_numbers();
    assert_eq!(bytes, b"1 system.a\n2 org.example.b\n");
    let mut restarted = store();
    assert_eq!(restarted.load_numbers(&bytes), 0);
    assert_eq!(restarted.app_id(2), Some("org.example.b"));
    assert_eq!(restarted.number("org.example.b"), (2, false));
    assert_eq!(restarted.number("system.c"), (3, true));
    // Numbers and app IDs that are taken, 0 and malformed lines are left
    // out.
    assert_eq!(
        restarted.load_numbers(b"3 other\n4 system.a\n0 x\nnone\n5 \n"),
        5
    );
    assert_eq!(restarted.app_id(3), Some("system.c"));
}

#[test]
fn grants_survive_a_restart() {
    let mut s = store();
    s.set_grant(&app_grant(
        1000,
        "org.example.b",
        "user.pictures.access",
        Decision::Allowed,
    ))
    .unwrap();
    s.set_grant(&app_grant(
        1001,
        "org.example.c",
        "user.pictures.access",
        Decision::Denied,
    ))
    .unwrap();
    let bytes = s.encode_grants();
    assert_eq!(
        bytes,
        b"1000 org.example.b user.pictures.access allowed\n\
          1001 org.example.c user.pictures.access denied\n"
    );
    let mut restarted = store();
    assert_eq!(restarted.load_grants(&bytes), 0);
    assert_eq!(restarted.list(1000), s.list(1000));
    assert_eq!(restarted.list(1001), s.list(1001));
    // A grant made before the stored ones were read stays, and grants of
    // system and admin permissions, unknown permissions and malformed
    // lines are left out.
    let mut early = store();
    early
        .set_grant(&app_grant(
            1000,
            "org.example.b",
            "user.pictures.access",
            Decision::Denied,
        ))
        .unwrap();
    let odd = b"1000 org.example.b user.pictures.access allowed\n\
        1000 x system.permissions.read allowed\n\
        1000 x admin.power.control allowed\n\
        1000 x no.such.permission allowed\n\
        1000 x user.pictures.access maybe\n\
        x y z allowed\n";
    assert_eq!(early.load_grants(odd), 6);
    assert_eq!(early.list(1000)[0].decision, Decision::Denied);
}

fn app_grant(user: u32, app_id: &str, permission: &str, decision: Decision) -> Grant {
    Grant {
        app_id: app_id.to_string(),
        ..grant(user, permission, decision)
    }
}

#[test]
fn keeps_portable_apps_grants_apart_and_never_a_builds() {
    let mut s = store();
    let portable = Identity {
        checksum: "sha256:ab",
        ..id(APP)
    };
    let build = Identity {
        checksum: BUILD,
        ..id(APP)
    };
    s.record(portable, USER_PERMISSION, true);
    assert_eq!(
        s.check(portable, USER_PERMISSION),
        Ok((Decision::Allowed, false))
    );
    // The installed app with the same ID and a build have none of it.
    assert_eq!(
        s.check(id(APP), USER_PERMISSION),
        Ok((Decision::Undetermined, false))
    );
    assert_eq!(
        s.check(build, USER_PERMISSION),
        Ok((Decision::Undetermined, false))
    );
    // A build holds what it was allowed only in its process, and keeps no
    // denial.
    s.record(build, USER_PERMISSION, true);
    assert_eq!(
        s.check(build, USER_PERMISSION),
        Ok((Decision::Allowed, false))
    );
    let later = Identity {
        koid: KOID + 1,
        ..build
    };
    assert_eq!(
        s.check(later, USER_PERMISSION),
        Ok((Decision::Undetermined, false))
    );
    s.record(later, DEFAULT_PERMISSION, false);
    assert_eq!(
        s.check(later, DEFAULT_PERMISSION),
        Ok((Decision::Allowed, false))
    );
    // Only the portable app's grant is written, with its checksum, and
    // read back as its own.
    let text = String::from_utf8(s.encode_grants()).unwrap();
    assert_eq!(
        text,
        std::format!("1 {APP} {USER_PERMISSION} allowed sha256:ab\n")
    );
    let mut loaded = store();
    assert_eq!(loaded.load_grants(text.as_bytes()), 0);
    assert_eq!(
        loaded.check(portable, USER_PERMISSION),
        Ok((Decision::Allowed, false))
    );
    assert_eq!(
        loaded.check(id(APP), USER_PERMISSION),
        Ok((Decision::Undetermined, false))
    );
    assert_eq!(
        loaded.load_grants(b"1 org.x user.camera.use allowed build\n"),
        1
    );
    // Neither has the system permissions of an app ID.
    s.grant_system(APP, &strings(&["admin.logs.read"])).unwrap();
    assert_eq!(
        s.check(id(APP), "admin.logs.read"),
        Ok((Decision::Allowed, false))
    );
    assert_eq!(
        s.check(portable, "admin.logs.read"),
        Ok((Decision::Undetermined, false))
    );
}

#[test]
fn a_question_names_the_service_that_asks_for_a_client() {
    assert_eq!(via(APP, 5, 5), "");
    assert_eq!(via("system.devicemgr", 5, 6), "system.devicemgr");
    assert_eq!(via(PROCESS_MANAGER, 5, 6), "");
}

#[test]
fn full_disk_answers_for_the_folders_whatever_their_grants() {
    let mut s = store();
    let check = |s: &Store, permission: &str| s.check(id(APP), permission).map(|d| d.0);
    for folder in FOLDERS {
        assert_eq!(check(&s, folder), Ok(Decision::Undetermined));
    }
    s.set_grant(&grant(1, FULL_DISK, Decision::Allowed))
        .unwrap();
    s.set_grant(&grant(1, "user.pictures.access", Decision::Denied))
        .unwrap();
    for folder in FOLDERS {
        assert_eq!(check(&s, folder), Ok(Decision::Allowed));
        let asked = s.request(id(APP), folder).map(|r| r.0);
        assert_eq!(asked, Ok(Requested::Decided(Decision::Allowed)));
    }
    // Neither the volumes nor another user's app are covered.
    assert_eq!(check(&s, "user.volumes.access"), Ok(Decision::Undetermined));
    let other = Identity { user: 2, ..id(APP) };
    assert_eq!(
        s.check(other, FOLDERS[0]).map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    s.set_grant(&grant(1, FULL_DISK, Decision::Denied)).unwrap();
    assert_eq!(check(&s, "user.pictures.access"), Ok(Decision::Denied));
}

#[test]
fn a_build_gets_the_folders_through_the_full_disk_it_holds() {
    let mut s = store();
    let build = Identity {
        checksum: "build",
        ..id(APP)
    };
    assert_eq!(
        s.check(build, FOLDERS[1]).map(|d| d.0),
        Ok(Decision::Undetermined)
    );
    s.record(build, FULL_DISK, true);
    assert_eq!(
        s.check(build, FOLDERS[1]).map(|d| d.0),
        Ok(Decision::Allowed)
    );
    let other_process = Identity {
        koid: KOID + 1,
        ..build
    };
    assert_eq!(
        s.check(other_process, FOLDERS[1]).map(|d| d.0),
        Ok(Decision::Undetermined)
    );
}

#[test]
fn taking_full_disk_away_revokes_the_folders_without_a_grant_of_their_own() {
    let mut s = store();
    s.set_grant(&grant(1, FULL_DISK, Decision::Allowed))
        .unwrap();
    s.set_grant(&grant(1, "user.music.access", Decision::Allowed))
        .unwrap();
    let revoked = s
        .set_grant_revoking(&grant(1, FULL_DISK, Decision::Denied))
        .unwrap();
    let names: Vec<&str> = revoked.iter().map(|r| r.permission.as_str()).collect();
    assert_eq!(names.len(), 7);
    assert_eq!(names[0], FULL_DISK);
    assert!(!names.contains(&"user.music.access"));
    assert!(names.contains(&"user.documents.access"));
    assert!(revoked.iter().all(|r| r.user == 1 && r.app_id == APP));
    // Nothing is taken away when it was not allowed.
    let again = s
        .set_grant_revoking(&grant(1, FULL_DISK, Decision::Denied))
        .unwrap();
    assert!(again.is_empty());
}

const LISTED: &str = "user.power.control";
/// An app with a grant list.
const LIST_APP: &str = "system.mikros.listed";

fn id_of<'a>(user: u32, app_id: &'a str, checksum: &'static str) -> Identity<'a> {
    Identity {
        user,
        checksum,
        ..id(app_id)
    }
}

fn answer(s: &Store, client: Identity<'_>, permission: &str) -> Decision {
    s.check(client, permission).unwrap().0
}

#[test]
fn a_manifest_grants_a_user_permission_to_every_user() {
    let mut s = store();
    s.grant_system(APP, &strings(&[USER_PERMISSION])).unwrap();
    for user in [0, 1, 1000] {
        let c = id_of(user, APP, "");
        assert_eq!(answer(&s, c, USER_PERMISSION), Decision::Allowed);
        assert_eq!(
            s.request(c, USER_PERMISSION),
            Ok((Requested::Decided(Decision::Allowed), false))
        );
    }
    // Neither a portable app nor a build has it, and another app does not.
    assert_ne!(
        answer(&s, id_of(1, APP, "abc"), USER_PERMISSION),
        Decision::Allowed
    );
    assert_ne!(
        answer(&s, id_of(1, APP, BUILD), USER_PERMISSION),
        Decision::Allowed
    );
    assert_ne!(
        answer(&s, id_of(1, "org.other", ""), USER_PERMISSION),
        Decision::Allowed
    );
}

#[test]
fn a_grant_list_grants_every_scope_to_every_user_but_zero() {
    let mut s = store();
    let names = strings(&[LISTED, "admin.power.control", "system.prompts.use"]);
    s.grant_list(LIST_APP, &names).unwrap();
    for name in &names {
        for user in [1, 2, 1000] {
            let c = id_of(user, LIST_APP, "");
            assert_eq!(answer(&s, c, name), Decision::Allowed, "{name}");
            assert_eq!(
                s.request(c, name),
                Ok((Requested::Decided(Decision::Allowed), false))
            );
        }
        // Neither user ID 0, nor a portable app, nor a build gets it.
        for client in [
            id_of(0, LIST_APP, ""),
            id_of(1, LIST_APP, "abc"),
            id_of(1, LIST_APP, BUILD),
        ] {
            assert_ne!(answer(&s, client, name), Decision::Allowed, "{name}");
        }
    }
    // As for a permission that nothing grants: a user permission is asked,
    // an admin one authorized, and a system one is denied without a prompt.
    let zero = id_of(0, LIST_APP, "");
    assert_eq!(s.request(zero, LISTED), Ok((Requested::Ask, false)));
    assert_eq!(
        s.request(zero, "admin.power.control"),
        Ok((Requested::Authorize, false))
    );
    assert_eq!(
        s.request(zero, "system.prompts.use"),
        Ok((Requested::Decided(Decision::Denied), false))
    );
}

#[test]
fn a_manifest_does_not_undo_the_exclusion_of_a_list() {
    let mut s = store();
    s.grant_list(LIST_APP, &strings(&[LISTED])).unwrap();
    s.grant_system(LIST_APP, &strings(&[LISTED])).unwrap();
    assert_eq!(
        answer(&s, id_of(1, LIST_APP, ""), LISTED),
        Decision::Allowed
    );
    assert_eq!(
        answer(&s, id_of(0, LIST_APP, ""), LISTED),
        Decision::Undetermined
    );
}

#[test]
fn a_grant_list_wins_over_a_stored_grant_and_leaves_it_alone() {
    let mut s = store();
    let denied = Grant {
        app_id: LIST_APP.into(),
        ..grant(1, USER_PERMISSION, Decision::Denied)
    };
    s.set_grant(&denied).unwrap();
    s.grant_list(LIST_APP, &strings(&[USER_PERMISSION]))
        .unwrap();
    assert_eq!(
        answer(&s, id_of(1, LIST_APP, ""), USER_PERMISSION),
        Decision::Allowed
    );
    // The stored grant is still there, and applies to the excluded.
    assert_eq!(s.list(1).len(), 1);
    assert_eq!(
        answer(&s, id_of(0, LIST_APP, ""), USER_PERMISSION),
        Decision::Undetermined
    );
    let again = Grant { user: 0, ..denied };
    assert_eq!(s.set_grant(&again), Err(Failure::InvalidArgs));
}

#[test]
fn set_grant_fails_for_what_a_manifest_or_a_list_grants() {
    let mut s = store();
    s.grant_system(APP, &strings(&[USER_PERMISSION])).unwrap();
    s.grant_list(LIST_APP, &strings(&[LISTED])).unwrap();
    let set = |app: &str, permission: &str, decision| Grant {
        app_id: app.to_string(),
        ..grant(1, permission, decision)
    };
    for decision in [Decision::Allowed, Decision::Denied, Decision::Undetermined] {
        assert_eq!(
            s.set_grant(&set(APP, USER_PERMISSION, decision)),
            Err(Failure::InvalidArgs)
        );
        assert_eq!(
            s.set_grant_revoking(&set(LIST_APP, LISTED, decision)),
            Err(Failure::InvalidArgs)
        );
    }
    assert!(s.is_registered(APP, USER_PERMISSION));
    assert!(s.is_registered(LIST_APP, LISTED));
    assert!(!s.is_registered(APP, "user.shares.use"));
    // Other permissions of the same apps are still settable.
    assert!(
        s.set_grant(&set(APP, "user.shares.use", Decision::Allowed))
            .is_ok()
    );
    assert!(s.list(1).iter().all(|g| g.permission != LISTED));
}

#[test]
fn grant_lists_are_not_written_to_the_grants_file() {
    let mut s = store();
    s.grant_list(LIST_APP, &strings(&[LISTED])).unwrap();
    s.grant_system(APP, &strings(&[USER_PERMISSION])).unwrap();
    assert!(s.list(1).is_empty());
}

#[test]
fn a_grant_list_is_checked_whole_and_replaces_the_earlier_one() {
    let mut s = store();
    let bad = |names: &[&str], app: &str| store().grant_list(app, &strings(names));
    assert_eq!(bad(&[LISTED], APP), Err(Failure::InvalidArgs));
    assert_eq!(bad(&["nope"], LIST_APP), Err(Failure::InvalidArgs));
    assert_eq!(
        bad(&[DEFAULT_PERMISSION], LIST_APP),
        Err(Failure::InvalidArgs)
    );
    assert_eq!(
        bad(&["user.full_disk.access"], LIST_APP),
        Err(Failure::InvalidArgs)
    );
    assert_eq!(
        bad(&["admin.grants.manage"], LIST_APP),
        Err(Failure::InvalidArgs)
    );
    assert_eq!(
        bad(&["system.devices.manage"], LIST_APP),
        Err(Failure::InvalidArgs)
    );
    assert_eq!(bad(&[LISTED, "nope"], LIST_APP), Err(Failure::InvalidArgs));
    let listed = |s: &Store| {
        let all = s.system_grants(("system.mikros.", ""));
        all.iter().filter(|(a, _)| a == LIST_APP).count()
    };
    assert_eq!(listed(&s), 0);
    s.grant_list(LIST_APP, &strings(&[LISTED, "admin.power.control"]))
        .unwrap();
    s.grant_list(LIST_APP, &strings(&[LISTED])).unwrap();
    assert_eq!(
        answer(&s, id_of(1, LIST_APP, ""), "admin.power.control"),
        Decision::Undetermined
    );
    assert_eq!(
        answer(&s, id_of(1, LIST_APP, ""), LISTED),
        Decision::Allowed
    );
}

#[test]
fn registrations_end_once() {
    let mut s = store();
    s.grant_list(LIST_APP, &strings(&[LISTED])).unwrap();
    assert_eq!(s.registrations_done(), Ok(()));
    assert_eq!(s.registrations_done(), Err(Failure::BadState));
    assert_eq!(
        s.grant_list(LIST_APP, &strings(&[])),
        Err(Failure::BadState)
    );
    // The list stays, and manifests are still registered.
    assert_eq!(
        answer(&s, id_of(1, LIST_APP, ""), LISTED),
        Decision::Allowed
    );
    s.grant_system(APP, &strings(&["admin.time.set"])).unwrap();
}

#[test]
fn system_grants_list_every_scope_in_order_and_by_page() {
    let mut s = store();
    s.grant_system("org.b", &strings(&["user.shares.use", "admin.time.set"]))
        .unwrap();
    s.grant_list("system.mikros.a", &strings(&[LISTED, "system.prompts.use"]))
        .unwrap();
    s.grant_system("org.a", &strings(&["admin.time.set", LISTED]))
        .unwrap();
    s.grant_system("system.mikros.a", &strings(&[LISTED]))
        .unwrap();
    let all = s.system_grants(("", ""));
    let pairs: Vec<(&str, &str)> = all.iter().map(|(a, p)| (a.as_str(), p.as_str())).collect();
    let ours: Vec<_> = pairs
        .iter()
        .filter(|(a, _)| a.starts_with("org.") || a.starts_with("system.mikros."))
        .collect();
    assert_eq!(
        ours,
        [
            &("org.a", "admin.time.set"),
            &("org.a", LISTED),
            &("org.b", "admin.time.set"),
            &("org.b", "user.shares.use"),
            &("system.mikros.a", "system.prompts.use"),
            &("system.mikros.a", LISTED),
        ]
    );
    assert!(pairs.windows(2).all(|w| w[0] < w[1]));
    // Paging: each page starts after the last pair of the one before.
    let mut seen = Vec::new();
    let mut after = (String::new(), String::new());
    loop {
        let page = s.system_grants((&after.0, &after.1));
        let Some(last) = page.last().cloned() else {
            break;
        };
        seen.extend(page);
        after = last;
    }
    assert_eq!(seen, all);
    assert!(s.system_grants(("~", "")).is_empty());
}
