extern crate std;

use crate::*;
use alloc::format;
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
            namespace: vec![],
            priority: 16,
            stop_timeout: 10,
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
stop_timeout: 600
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
    assert_eq!(
        s.namespace,
        [NamespaceEntry {
            path: "/dev".to_string(),
            rights: "r".to_string(),
        }]
    );
    assert_eq!(s.priority, 20);
    assert_eq!(s.stop_timeout, 600);
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
fn stop_timeout_must_be_a_positive_integer() {
    let with = |extra: &str| std::format!("{MINIMAL}{extra}\n");
    let out_of_range = ErrorKind::Yaml(mikros_yaml::ErrorKind::OutOfRange);
    let wrong = ErrorKind::Yaml(mikros_yaml::ErrorKind::WrongType);
    assert_eq!(kind("log.yaml", &with("stop_timeout: 0")), out_of_range);
    assert_eq!(kind("log.yaml", &with("stop_timeout: -5")), out_of_range);
    assert_eq!(kind("log.yaml", &with("stop_timeout: '5'")), wrong);
    assert_eq!(kind("log.yaml", &with("stop_timeout: 1.5")), wrong);
    let s = manifest("log.yaml", &with("stop_timeout: 1")).service;
    assert_eq!(s.stop_timeout, 1);
}

#[test]
fn priority_is_read_without_a_range() {
    let s = manifest("log.yaml", &std::format!("{MINIMAL}priority: 40\n")).service;
    assert_eq!(s.priority, 40);
}

const PROGRAM: &str = "app_id: system.console
binary: /usr/bin/console
permissions:
  - devices.framebuffer
namespace:
  - path: /usr
    rights: rx
spawned_by:
  - system.service-manager
";

#[test]
fn reads_a_program_manifest() {
    let read = parse_program("system.console.yaml", PROGRAM.as_bytes()).unwrap();
    let p = read.program;
    assert_eq!(p.app_id, "system.console");
    assert_eq!(p.binary, "/usr/bin/console");
    assert_eq!(p.permissions, ["devices.framebuffer"]);
    assert_eq!(
        p.namespace,
        [NamespaceEntry {
            path: "/usr".into(),
            rights: "rx".into()
        }]
    );
    assert_eq!(p.spawned_by, ["system.service-manager"]);
    assert!(read.unknown.is_empty());
}

#[test]
fn refuses_program_manifests_that_break_the_rules() {
    let kind = |file: &str, text: &str| {
        parse_program(file, text.as_bytes())
            .map(drop)
            .map_err(|e| e.kind)
    };
    let with = |id: &str| alloc::format!("app_id: {id}\nbinary: /usr/bin/x\n");
    assert_eq!(
        kind("other.yaml", &with("system.console")),
        Err(ErrorKind::FileName)
    );
    for id in ["test.app", "system.cli", "system.driver.gpu"] {
        let file = alloc::format!("{id}.yaml");
        assert_eq!(kind(&file, &with(id)), Err(ErrorKind::BadAppId), "{id}");
    }
    let outside = "app_id: system.x\nbinary: /Apps/x/bin\n";
    assert_eq!(kind("system.x.yaml", outside), Err(ErrorKind::BadBinary));
    assert_eq!(
        kind("system.x.yaml", "app_id: system.x\n"),
        Err(ErrorKind::Missing("binary"))
    );
    assert_eq!(kind("system.x.yaml", &with("system.x")), Ok(()));
}

#[test]
fn reads_a_driver_manifest() {
    let text = "name: pci-bus\nbinary: /usr/lib/drivers/pci-bus/pci-bus\n\
                match: [platform:pci-host-ecam]\nkind: normal\nneeds: [bus, dma]\n\
                permissions: [device.registry]\npriority: 20\nfoo: 1\n";
    let dir = "pci-bus";
    let m = parse_driver(dir, text.as_bytes()).unwrap();
    let d = &m.driver;
    assert_eq!(
        (d.name.as_str(), d.binary.as_str()),
        ("pci-bus", "/usr/lib/drivers/pci-bus/pci-bus")
    );
    assert_eq!(d.matches, ["platform:pci-host-ecam"]);
    assert_eq!(
        (d.kind, d.needs.as_slice()),
        (DriverKind::Normal, &[Need::Bus, Need::Dma][..])
    );
    assert_eq!(
        (d.permissions.as_slice(), d.priority),
        (&["device.registry".into()][..], 20)
    );
    assert_eq!(m.unknown.len(), 1);
    let fallback = parse_driver("a", b"name: a\nbinary: /usr/a\nkind: fallback\n").unwrap();
    assert_eq!(fallback.driver.kind, DriverKind::Fallback);
    let minimal = parse_driver("a", b"name: a\nbinary: /usr/a\n")
        .unwrap()
        .driver;
    assert_eq!(
        (minimal.kind, minimal.priority),
        (DriverKind::Normal, PRIORITY_DEFAULT)
    );
    assert!(minimal.matches.is_empty() && minimal.needs.is_empty());
    assert!(minimal.permissions.is_empty() && minimal.namespace.is_empty());
}

#[test]
fn refuses_driver_manifests_that_break_the_rules() {
    let dir = "a";
    let kind_of = |text: &str| parse_driver(dir, text.as_bytes()).unwrap_err().kind;
    let base = "name: a\nbinary: /usr/a\nkind: normal\n";
    assert_eq!(
        kind_of(&format!("{base}start: boot\n")),
        ErrorKind::ServiceKey("start")
    );
    assert_eq!(
        kind_of(&format!("{base}provides: []\n")),
        ErrorKind::ServiceKey("provides")
    );
    assert_eq!(
        kind_of("name: a\nbinary: /usr/a\nkind: odd\n"),
        ErrorKind::BadKind
    );
    assert_eq!(
        kind_of(&format!("{base}needs: [gpu]\n")),
        ErrorKind::BadNeed
    );
    assert_eq!(
        kind_of(&format!("{base}match: [Pci:1]\n")),
        ErrorKind::BadHardwareId
    );
    assert_eq!(
        kind_of(&format!("{base}permissions: [dma.map, logs.add_source]\n")),
        ErrorKind::ReservedPermission
    );
    let e = parse_driver(
        dir,
        format!("{base}permissions: [logs.add_source]\n").as_bytes(),
    )
    .unwrap_err();
    assert_eq!((e.line, e.column), (4, 15));
    assert_eq!(kind_of("binary: /usr/a\n"), ErrorKind::Missing("name"));
    assert_eq!(kind_of("name: a\n"), ErrorKind::Missing("binary"));
    assert_eq!(
        kind_of("name: a\nbinary: /bin/a\nkind: normal\n"),
        ErrorKind::BadBinary
    );
    assert_eq!(
        kind_of("name: b\nbinary: /usr/b\nkind: normal\n"),
        ErrorKind::FileName
    );
}

#[test]
fn checks_hardware_ids() {
    assert!(is_hardware_id("pci:8086:100e"));
    assert!(is_hardware_id("platform:pci-host-ecam"));
    assert!(is_hardware_id("usb:class_08"));
    assert!(!is_hardware_id("pci"));
    assert!(!is_hardware_id(":100e"));
    assert!(!is_hardware_id("pci:8086:100E"));
    assert!(!is_hardware_id("pci bus:1"));
}

fn driver(name: &str, kind: DriverKind, matches: &[&str]) -> Driver {
    Driver {
        name: name.into(),
        binary: format!("/usr/lib/drivers/{name}/{name}"),
        permissions: vec![],
        namespace: vec![],
        priority: PRIORITY_DEFAULT,
        matches: matches.iter().map(ToString::to_string).collect(),
        kind,
        needs: vec![],
    }
}

#[test]
fn chooses_drivers_by_hardware_id_kind_and_name() {
    use DriverKind::{Fallback, Normal};
    let drivers = [
        driver("vga", Fallback, &["pci:class_0300"]),
        driver("virtio-gpu", Normal, &["pci:1af4:1050"]),
        driver("bochs", Normal, &["pci:1234:1111", "pci:class_0300"]),
        driver("any-vga", Normal, &["pci:class_0300"]),
        driver("generic", Fallback, &["pci:1af4:1050"]),
    ];
    let ids = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
    let chosen = |hardware_ids: &[String]| {
        choose(&drivers, hardware_ids).map(|c| {
            (
                c.driver.name.clone(),
                c.hardware_id.to_string(),
                c.passed_over.len(),
            )
        })
    };
    // A normal driver for a later ID beats a fallback for the first.
    let gpu = ids(&["pci:1af4:1050", "pci:class_0300"]);
    assert_eq!(
        chosen(&gpu),
        Some(("virtio-gpu".into(), "pci:1af4:1050".into(), 0))
    );
    // The most specific ID wins over a less specific one.
    let bochs = ids(&["pci:1234:1111", "pci:class_0300"]);
    assert_eq!(
        chosen(&bochs),
        Some(("bochs".into(), "pci:1234:1111".into(), 0))
    );
    // Of two normal drivers for one ID, the first by name.
    let vga = ids(&["pci:8086:0001", "pci:class_0300"]);
    let choice = choose(&drivers, &vga).unwrap();
    assert_eq!(
        (choice.driver.name.as_str(), choice.passed_over),
        ("any-vga", vec!["bochs"])
    );
    // A fallback when no normal driver matches, none when nothing does.
    let only_fallback = [driver("vga", Fallback, &["pci:class_0300"])];
    assert_eq!(choose(&only_fallback, &vga).unwrap().driver.name, "vga");
    assert_eq!(chosen(&ids(&["usb:1:2"])), None);
}
