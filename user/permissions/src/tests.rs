extern crate std;

use crate::*;
use alloc::string::ToString;
use alloc::vec;

const BROKER: &str = "system.permissions";
const APP: &str = "org.example.app";

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn store() -> Store {
    Store::bootstrap(BROKER, &strings(&["logs.read"])).unwrap()
}

fn id(app_id: &str) -> Identity<'_> {
    Identity { user: 1, app_id }
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
    assert_eq!(PERMISSIONS.len(), 50);
    assert_eq!(kind("camera"), Some(Kind::User));
    assert_eq!(kind("network"), Some(Kind::Default));
    assert_eq!(kind("permissions.check"), Some(Kind::System));
    assert_eq!(kind("gpu.high_priority"), Some(Kind::System));
    assert_eq!(kind("nope"), None);
}

#[test]
fn bootstrap_trusts_the_service_manager_and_the_broker_manifest() {
    let s = store();
    for p in SERVICE_MANAGER_PERMISSIONS {
        assert_eq!(s.check(id(SERVICE_MANAGER), p), Ok(Decision::Allowed));
    }
    assert_eq!(s.authorize(SERVICE_MANAGER, Call::Check), Ok(()));
    assert_eq!(s.authorize(SERVICE_MANAGER, Call::GrantSystem), Ok(()));
    assert_eq!(
        s.authorize(SERVICE_MANAGER, Call::SetGrant),
        Err(Failure::AccessDenied)
    );
    assert_eq!(s.check(id(BROKER), "logs.read"), Ok(Decision::Allowed));
}

#[test]
fn bootstrap_refuses_a_bad_manifest() {
    assert_eq!(
        Store::bootstrap(BROKER, &strings(&["camera"])).err(),
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
        (Call::Check, "permissions.check"),
        (Call::Request, "permissions.check"),
        (Call::Watch, "permissions.check"),
        (Call::SetGrant, "permissions.manage"),
        (Call::ListGrants, "permissions.manage"),
        (Call::GrantSystem, "permissions.grant_system"),
        (Call::RegisterPrompter, "permissions.prompt"),
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
        s.check(id(APP), "device.registry"),
        Ok(Decision::Undetermined)
    );
    s.grant_system(APP, &strings(&["device.registry"])).unwrap();
    for user in [0, 1, 1000] {
        let who = Identity { user, app_id: APP };
        assert_eq!(s.check(who, "device.registry"), Ok(Decision::Allowed));
    }
}

#[test]
fn grant_system_adds_and_checks_every_name_first() {
    let mut s = store();
    s.grant_system(APP, &strings(&["logs.read"])).unwrap();
    s.grant_system(APP, &strings(&["time.set"])).unwrap();
    assert_eq!(s.check(id(APP), "logs.read"), Ok(Decision::Allowed));
    assert_eq!(s.check(id(APP), "time.set"), Ok(Decision::Allowed));
    let bad = strings(&["power.control", "camera"]);
    assert_eq!(s.grant_system(APP, &bad), Err(Failure::InvalidArgs));
    let unknown = strings(&["power.control", "nope"]);
    assert_eq!(s.grant_system(APP, &unknown), Err(Failure::NotFound));
    assert_eq!(
        s.check(id(APP), "power.control"),
        Ok(Decision::Undetermined)
    );
}

#[test]
fn checks_by_kind() {
    let mut s = store();
    assert_eq!(s.check(id(APP), "camera"), Ok(Decision::Undetermined));
    assert_eq!(s.check(id(APP), "network"), Ok(Decision::Allowed));
    assert_eq!(s.check(id(APP), "nope"), Err(Failure::NotFound));
    s.set_grant(&grant(1, "camera", Decision::Allowed)).unwrap();
    s.set_grant(&grant(1, "network", Decision::Denied)).unwrap();
    assert_eq!(s.check(id(APP), "camera"), Ok(Decision::Allowed));
    assert_eq!(s.check(id(APP), "network"), Ok(Decision::Denied));
    // Another user's grants are their own.
    let other = Identity {
        user: 2,
        app_id: APP,
    };
    assert_eq!(s.check(other, "camera"), Ok(Decision::Undetermined));
    assert_eq!(s.check(other, "network"), Ok(Decision::Allowed));
    s.set_grant(&grant(1, "camera", Decision::Denied)).unwrap();
    assert_eq!(s.check(id(APP), "camera"), Ok(Decision::Denied));
}

#[test]
fn requests_ask_only_for_undetermined_user_permissions() {
    let mut s = store();
    assert_eq!(s.request(id(APP), "camera"), Ok(Requested::Ask));
    assert_eq!(
        s.request(id(APP), "network"),
        Ok(Requested::Decided(Decision::Allowed))
    );
    assert_eq!(
        s.request(id(APP), "device.registry"),
        Ok(Requested::Decided(Decision::Undetermined))
    );
    assert_eq!(s.request(id(APP), "nope"), Err(Failure::NotFound));
    assert_eq!(s.record(id(APP), "camera", false), Decision::Denied);
    assert_eq!(
        s.request(id(APP), "camera"),
        Ok(Requested::Decided(Decision::Denied))
    );
    assert_eq!(s.record(id(APP), "microphone", true), Decision::Allowed);
    assert_eq!(
        s.request(id(APP), "microphone"),
        Ok(Requested::Decided(Decision::Allowed))
    );
}

#[test]
fn set_grant_refuses_system_and_unknown_permissions() {
    let mut s = store();
    assert_eq!(
        s.set_grant(&grant(1, "time.set", Decision::Allowed)),
        Err(Failure::InvalidArgs)
    );
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
    assert_eq!(s.set_grant(&grant(1, "camera", Decision::Denied)), Ok(None));
    assert_eq!(
        s.set_grant(&grant(1, "camera", Decision::Allowed)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "camera", Decision::Undetermined)),
        Ok(revoked("camera"))
    );
    assert_eq!(
        s.set_grant(&grant(1, "camera", Decision::Allowed)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "camera", Decision::Denied)),
        Ok(revoked("camera"))
    );
    // Default permissions: a new DENIED grant too.
    assert_eq!(
        s.set_grant(&grant(1, "network", Decision::Denied)),
        Ok(revoked("network"))
    );
    assert_eq!(
        s.set_grant(&grant(1, "network", Decision::Denied)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "network", Decision::Allowed)),
        Ok(None)
    );
    assert_eq!(
        s.set_grant(&grant(1, "network", Decision::Denied)),
        Ok(revoked("network"))
    );
}

#[test]
fn undetermined_removes_a_grant() {
    let mut s = store();
    s.set_grant(&grant(1, "camera", Decision::Denied)).unwrap();
    s.set_grant(&grant(1, "camera", Decision::Undetermined))
        .unwrap();
    assert!(s.list(1).is_empty());
    assert_eq!(s.check(id(APP), "camera"), Ok(Decision::Undetermined));
}

#[test]
fn list_holds_one_users_grants_in_order() {
    let mut s = store();
    s.set_grant(&grant(1, "microphone", Decision::Allowed))
        .unwrap();
    s.set_grant(&grant(1, "camera", Decision::Denied)).unwrap();
    s.set_grant(&grant(2, "camera", Decision::Allowed)).unwrap();
    s.set_grant(&grant(0, "camera", Decision::Allowed)).unwrap();
    assert_eq!(
        s.list(1),
        vec![
            grant(1, "camera", Decision::Denied),
            grant(1, "microphone", Decision::Allowed),
        ]
    );
    assert!(s.list(3).is_empty());
}

#[test]
fn list_stops_at_its_limit() {
    let mut s = store();
    for i in 0..LIST_MAX + 10 {
        let mut g = grant(1, "camera", Decision::Allowed);
        g.app_id = std::format!("app.{i:04}");
        s.set_grant(&g).unwrap();
    }
    assert_eq!(s.list(1).len(), LIST_MAX);
}
