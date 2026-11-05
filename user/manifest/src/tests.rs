extern crate std;

use crate::*;
use alloc::string::ToString;
use alloc::vec;

fn manifest(file: &str, text: &str) -> Manifest {
    match parse(file, text.as_bytes()) {
        Ok(m) => m,
        Err(e) => panic!("{e:?} in {text:?}"),
    }
}

fn kind(file: &str, text: &str) -> ErrorKind {
    match parse(file, text.as_bytes()) {
        Ok(m) => panic!("accepted {text:?} as {m:?}"),
        Err(e) => e.kind,
    }
}

const MINIMAL: &str = "name: log\nbinary: /usr/bin/log\nstart: boot\n";

#[test]
fn minimal_manifest_takes_defaults() {
    let m = manifest("log.yaml", MINIMAL);
    assert_eq!(
        m.service,
        Service {
            name: "log".to_string(),
            binary: "/usr/bin/log".to_string(),
            provides: vec![],
            after: vec![],
            start: Start::Boot,
            restart: Restart::Never,
            boot_handles: vec![],
            permissions: vec![],
            priority: 16,
        }
    );
    assert!(m.unknown.is_empty());
}

#[test]
fn full_manifest() {
    let text = "\
name: net-stack
binary: /usr/bin/net
start: on-demand
restart: on-failure
after: [log, devices]
provides:
  - protocol: mikros.net.Stack
    permission: network.use
  - protocol: mikros.net.Admin
boot_handles: [root_resource, log_manage, root_budget, framebuffer, platform_info]
permissions:
  - device.registry
namespace:
  - path: /dev
    rights: r
priority: 20
";
    let s = manifest("net-stack.yaml", text).service;
    assert_eq!(s.start, Start::OnDemand);
    assert_eq!(s.restart, Restart::OnFailure);
    assert_eq!(s.after, ["log", "devices"]);
    assert_eq!(
        s.provides,
        [
            Provide {
                protocol: "mikros.net.Stack".to_string(),
                permission: Some("network.use".to_string()),
            },
            Provide {
                protocol: "mikros.net.Admin".to_string(),
                permission: None,
            },
        ]
    );
    assert_eq!(
        s.boot_handles,
        [
            BootHandle::RootResource,
            BootHandle::LogManage,
            BootHandle::RootBudget,
            BootHandle::Framebuffer,
            BootHandle::PlatformInfo,
        ]
    );
    assert_eq!(s.permissions, ["device.registry"]);
    assert_eq!(s.priority, 20);
}

#[test]
fn absent_values_take_defaults() {
    let text = "name: log\nbinary: /usr/bin/log\nstart: boot\nrestart: ~\nafter: null\npriority:\n";
    let s = manifest("log.yaml", text).service;
    assert_eq!(s.restart, Restart::Never);
    assert!(s.after.is_empty());
    assert_eq!(s.priority, 16);
}

#[test]
fn unknown_keys_are_reported_with_their_place() {
    let text = "name: log\nbinary: /usr/bin/log\nstart: boot\ncolour: red\nprovides:\n  - protocol: p\n    extra: 1\n";
    let unknown = manifest("log.yaml", text).unknown;
    let keys: alloc::vec::Vec<_> = unknown.iter().map(|u| (u.key.as_str(), u.line)).collect();
    assert_eq!(keys, [("colour", 4), ("extra", 7)]);
}

#[test]
fn required_keys() {
    assert_eq!(
        kind("log.yaml", "binary: /usr/bin/log\nstart: boot\n"),
        ErrorKind::Missing("name")
    );
    assert_eq!(
        kind("log.yaml", "name: log\nstart: boot\n"),
        ErrorKind::Missing("binary")
    );
    assert_eq!(
        kind("log.yaml", "name: log\nbinary: /usr/bin/log\n"),
        ErrorKind::Missing("start")
    );
    assert_eq!(
        kind("log.yaml", "name: log\nbinary: /usr/bin/log\nstart: ~\n"),
        ErrorKind::Missing("start")
    );
}

#[test]
fn names() {
    for good in ["a", "log", "net-stack", "a1-b2-c3", "x-1"] {
        let text = std::format!("name: {good}\nbinary: /usr/bin/x\nstart: boot\n");
        manifest(&std::format!("{good}.yaml"), &text);
    }
    for bad in ["1a", "-a", "a-", "a--b", "Log", "a_b", "a.b", "'é'"] {
        let text = std::format!("name: {bad}\nbinary: /usr/bin/x\nstart: boot\n");
        assert_eq!(kind("x.yaml", &text), ErrorKind::BadName, "{bad}");
    }
}

#[test]
fn file_must_be_named_after_the_service() {
    assert_eq!(kind("other.yaml", MINIMAL), ErrorKind::FileName);
    assert_eq!(kind("log", MINIMAL), ErrorKind::FileName);
    assert_eq!(kind("log.yml", MINIMAL), ErrorKind::FileName);
}

#[test]
fn binary_must_be_under_usr() {
    for bad in [
        "usr/bin/x",
        "/bin/x",
        "/usr",
        "/usr/",
        "/usrx/bin",
        "/usr/../bin/x",
        "/usr/./x",
        "/usr//x",
        "/usr/bin/",
    ] {
        let text = std::format!("name: log\nbinary: '{bad}'\nstart: boot\n");
        assert_eq!(kind("log.yaml", &text), ErrorKind::BadBinary, "{bad}");
    }
}

#[test]
fn bad_values() {
    let with = |extra: &str| std::format!("{MINIMAL}{extra}\n");
    assert_eq!(
        kind("log.yaml", &with("boot_handles: [log]")),
        ErrorKind::BadBootHandle
    );
    assert_eq!(
        kind("log.yaml", &with("restart: sometimes")),
        ErrorKind::BadRestart
    );
    assert_eq!(
        kind(
            "log.yaml",
            "name: log\nbinary: /usr/bin/log\nstart: later\n"
        ),
        ErrorKind::BadStart
    );
    assert_eq!(
        kind("log.yaml", &with("provides:\n  - permission: p")),
        ErrorKind::Missing("protocol")
    );
}

#[test]
fn wrong_types_make_the_file_invalid() {
    let wrong = ErrorKind::Yaml(mikros_yaml::ErrorKind::WrongType);
    let with = |extra: &str| std::format!("{MINIMAL}{extra}\n");
    assert_eq!(kind("log.yaml", &with("after: log")), wrong);
    assert_eq!(kind("log.yaml", &with("permissions: [[a]]")), wrong);
    assert_eq!(kind("log.yaml", &with("provides: [p]")), wrong);
    assert_eq!(kind("log.yaml", &with("priority: '17'")), wrong);
    assert_eq!(kind("log.yaml", &with("priority: high")), wrong);
    assert_eq!(
        kind(
            "log.yaml",
            "name: [log]\nbinary: /usr/bin/log\nstart: boot\n"
        ),
        wrong
    );
}

#[test]
fn errors_say_where() {
    let e = parse("log.yaml", b"name: log\nbinary: /bin/log\nstart: boot\n").unwrap_err();
    assert_eq!((e.line, e.column), (2, 9));
    let e = parse("log.yaml", b"name: log\nstart: boot\n").unwrap_err();
    assert_eq!((e.line, e.column), (1, 1));
    let e = parse("log.yaml", b"name: [log\n").unwrap_err();
    assert!(matches!(e.kind, ErrorKind::Yaml(_)));
}

#[test]
fn priority_is_read_without_a_range() {
    let s = manifest("log.yaml", &std::format!("{MINIMAL}priority: 40\n")).service;
    assert_eq!(s.priority, 40);
}
