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

const MINIMAL: &str = "name: log\nbinary: /system/bin/log\nstart: boot\n";

#[test]
fn minimal_manifest_takes_defaults() {
    let m = manifest("system.log", MINIMAL);
    assert_eq!(
        m.service,
        Service {
            name: "log".to_string(),
            app_id: "system.log".to_string(),
            binary: "/system/bin/log".to_string(),
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
binary: /system/bin/net
start: on-demand
restart: on-failure
after: [log, devices]
provides:
  - protocol: mikros.net.Stack
    permission: network.use
  - protocol: mikros.net.Admin
boot_handles: [root_resource, log_manage, root_budget, framebuffer, platform_info]
permissions:
  - system.devices.manage
namespace:
  - path: /dev
    rights: r
priority: 20
stop_timeout: 600
";
    let s = manifest("system.net-stack", text).service;
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
    assert_eq!(s.permissions, ["system.devices.manage"]);
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
    let text =
        "name: log\nbinary: /system/bin/log\nstart: boot\nrestart: ~\nafter: null\npriority:\n";
    let s = manifest("system.log", text).service;
    assert_eq!(s.restart, Restart::Never);
    assert!(s.after.is_empty());
    assert_eq!(s.priority, 16);
}

#[test]
fn unknown_keys_are_reported_with_their_place() {
    let text = "name: log\nbinary: /system/bin/log\nstart: boot\ncolour: red\nprovides:\n  - protocol: p\n    extra: 1\n";
    let unknown = manifest("system.log", text).unknown;
    let keys: alloc::vec::Vec<_> = unknown.iter().map(|u| (u.key.as_str(), u.line)).collect();
    assert_eq!(keys, [("colour", 4), ("extra", 7)]);
}

#[test]
fn required_keys() {
    assert_eq!(
        kind("system.log", "binary: /system/bin/log\nstart: boot\n"),
        ErrorKind::Missing("name")
    );
    assert_eq!(
        kind("system.log", "name: log\nstart: boot\n"),
        ErrorKind::Missing("binary")
    );
    assert_eq!(
        kind("system.log", "name: log\nbinary: /system/bin/log\n"),
        ErrorKind::Missing("start")
    );
    assert_eq!(
        kind(
            "system.log",
            "name: log\nbinary: /system/bin/log\nstart: ~\n"
        ),
        ErrorKind::Missing("start")
    );
}

#[test]
fn names() {
    for good in ["a", "log", "net-stack", "a1-b2-c3", "x-1"] {
        let text = std::format!("name: {good}\nbinary: /system/bin/x\nstart: boot\n");
        manifest(&std::format!("system.{good}"), &text);
    }
    for bad in ["1a", "-a", "a-", "a--b", "Log", "a_b", "a.b", "'é'"] {
        let text = std::format!("name: {bad}\nbinary: /system/bin/x\nstart: boot\n");
        assert_eq!(kind("system.x", &text), ErrorKind::BadName, "{bad}");
    }
}

#[test]
fn folder_must_be_named_by_the_app_id() {
    assert_eq!(kind("system.other", MINIMAL), ErrorKind::FileName);
    assert_eq!(kind("log", MINIMAL), ErrorKind::FileName);
    assert_eq!(kind("log.yaml", MINIMAL), ErrorKind::FileName);
    assert_eq!(kind("system.log.yaml", MINIMAL), ErrorKind::FileName);
}

#[test]
fn binary_must_be_under_system() {
    for bad in [
        "system/bin/x",
        "/bin/x",
        "/usr/bin/x",
        "/system",
        "/system/",
        "/systemx/bin",
        "/system/../bin/x",
        "/system/./x",
        "/system//x",
        "/system/bin/",
    ] {
        let text = std::format!("name: log\nbinary: '{bad}'\nstart: boot\n");
        assert_eq!(kind("system.log", &text), ErrorKind::BadBinary, "{bad}");
    }
}

#[test]
fn bad_values() {
    let with = |extra: &str| std::format!("{MINIMAL}{extra}\n");
    assert_eq!(
        kind("system.log", &with("boot_handles: [log]")),
        ErrorKind::BadBootHandle
    );
    assert_eq!(
        kind("system.log", &with("restart: sometimes")),
        ErrorKind::BadRestart
    );
    assert_eq!(
        kind(
            "system.log",
            "name: log\nbinary: /system/bin/log\nstart: later\n"
        ),
        ErrorKind::BadStart
    );
    assert_eq!(
        kind("system.log", &with("provides:\n  - permission: p")),
        ErrorKind::Missing("protocol")
    );
}

#[test]
fn wrong_types_make_the_file_invalid() {
    let wrong = ErrorKind::Yaml(mikros_yaml::ErrorKind::WrongType);
    let with = |extra: &str| std::format!("{MINIMAL}{extra}\n");
    assert_eq!(kind("system.log", &with("after: log")), wrong);
    assert_eq!(kind("system.log", &with("permissions: [[a]]")), wrong);
    assert_eq!(kind("system.log", &with("provides: [p]")), wrong);
    assert_eq!(kind("system.log", &with("priority: '17'")), wrong);
    assert_eq!(kind("system.log", &with("priority: high")), wrong);
    assert_eq!(
        kind(
            "system.log",
            "name: [log]\nbinary: /system/bin/log\nstart: boot\n"
        ),
        wrong
    );
}

#[test]
fn errors_say_where() {
    let e = parse("system.log", b"name: log\nbinary: /bin/log\nstart: boot\n").unwrap_err();
    assert_eq!((e.line, e.column), (2, 9));
    let e = parse("system.log", b"name: log\nstart: boot\n").unwrap_err();
    assert_eq!((e.line, e.column), (1, 1));
    let e = parse("system.log", b"name: [log\n").unwrap_err();
    assert!(matches!(e.kind, ErrorKind::Yaml(_)));
}

#[test]
fn stop_timeout_must_be_a_positive_integer() {
    let with = |extra: &str| std::format!("{MINIMAL}{extra}\n");
    let out_of_range = ErrorKind::Yaml(mikros_yaml::ErrorKind::OutOfRange);
    let wrong = ErrorKind::Yaml(mikros_yaml::ErrorKind::WrongType);
    assert_eq!(kind("system.log", &with("stop_timeout: 0")), out_of_range);
    assert_eq!(kind("system.log", &with("stop_timeout: -5")), out_of_range);
    assert_eq!(kind("system.log", &with("stop_timeout: '5'")), wrong);
    assert_eq!(kind("system.log", &with("stop_timeout: 1.5")), wrong);
    let s = manifest("system.log", &with("stop_timeout: 1")).service;
    assert_eq!(s.stop_timeout, 1);
}

#[test]
fn priority_is_read_without_a_range() {
    let s = manifest("system.log", &std::format!("{MINIMAL}priority: 40\n")).service;
    assert_eq!(s.priority, 40);
}

const PROGRAM: &str = "app_id: system.console
binary: /system/bin/console
permissions:
  - devices.framebuffer
namespace:
  - path: /system
    rights: rx
spawned_by:
  - system.servicemgr
";

#[test]
fn reads_a_program_manifest() {
    let read = parse_program("system.console.yaml", PROGRAM.as_bytes()).unwrap();
    let p = read.program;
    assert_eq!(p.app_id, "system.console");
    assert_eq!(p.binary, "/system/bin/console");
    assert_eq!(p.permissions, ["devices.framebuffer"]);
    assert_eq!(
        p.namespace,
        [NamespaceEntry {
            path: "/system".into(),
            rights: "rx".into()
        }]
    );
    assert_eq!(p.spawned_by, ["system.servicemgr"]);
    assert!(!p.command);
    assert!(read.unknown.is_empty());
}

#[test]
fn reads_and_checks_command_manifests() {
    let kind = |text: &str| {
        parse_program("system.ps.yaml", text.as_bytes())
            .map(|m| m.program.command)
            .map_err(|e| e.kind)
    };
    let command = "app_id: system.ps\nbinary: /system/bin/ps\ncommand: true\n";
    assert_eq!(kind(command), Ok(true));
    assert_eq!(
        kind("app_id: system.ps\nbinary: /system/bin/ps\ncommand: false\n"),
        Ok(false)
    );
    assert_eq!(
        kind("app_id: system.ps\nbinary: /system/libraries/ps\ncommand: true\n"),
        Err(ErrorKind::BadBinary)
    );
    for (key, line) in [
        ("namespace", "namespace: []"),
        ("spawned_by", "spawned_by: [x]"),
    ] {
        let text = alloc::format!("{command}{line}\n");
        assert_eq!(kind(&text), Err(ErrorKind::CommandKey(key)));
    }
    assert!(matches!(
        kind("app_id: system.ps\nbinary: /system/bin/ps\ncommand: yes please\n"),
        Err(ErrorKind::Yaml(_))
    ));
}

#[test]
fn refuses_program_manifests_that_break_the_rules() {
    let kind = |file: &str, text: &str| {
        parse_program(file, text.as_bytes())
            .map(drop)
            .map_err(|e| e.kind)
    };
    let with = |id: &str| alloc::format!("app_id: {id}\nbinary: /system/bin/x\n");
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
    let text = "name: pcibus\nbinary: /system/drivers/system.driver.pcibus/pcibus\n\
                match: [platform:pci-host-ecam]\nkind: normal\nneeds: [bus, dma]\n\
                permissions: [system.devices.manage]\npriority: 20\nfoo: 1\n";
    let dir = "system.driver.pcibus";
    let m = parse_driver(dir, text.as_bytes()).unwrap();
    let d = &m.driver;
    assert_eq!(
        (d.name.as_str(), d.binary.as_str()),
        ("pcibus", "/system/drivers/system.driver.pcibus/pcibus")
    );
    assert_eq!(d.matches, ["platform:pci-host-ecam"]);
    assert_eq!(
        (d.kind, d.needs.as_slice()),
        (DriverKind::Normal, &[Need::Bus, Need::Dma][..])
    );
    assert_eq!(
        (d.permissions.as_slice(), d.priority),
        (&["system.devices.manage".into()][..], 20)
    );
    assert_eq!(m.unknown.len(), 1);
    let fallback = parse_driver(
        "system.driver.a",
        b"name: a\nbinary: /system/a\nkind: fallback\n",
    )
    .unwrap();
    assert_eq!(fallback.driver.kind, DriverKind::Fallback);
    let minimal = parse_driver("system.driver.a", b"name: a\nbinary: /system/a\n")
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
    let dir = "system.driver.a";
    let kind_of = |text: &str| parse_driver(dir, text.as_bytes()).unwrap_err().kind;
    let base = "name: a\nbinary: /system/a\nkind: normal\n";
    assert_eq!(
        kind_of(&format!("{base}start: boot\n")),
        ErrorKind::ServiceKey("start")
    );
    assert_eq!(
        kind_of(&format!("{base}provides: []\n")),
        ErrorKind::ServiceKey("provides")
    );
    assert_eq!(
        kind_of("name: a\nbinary: /system/a\nkind: odd\n"),
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
        kind_of(&format!(
            "{base}permissions: [dma.map, system.log_sources.create]\n"
        )),
        ErrorKind::ReservedPermission
    );
    let e = parse_driver(
        dir,
        format!("{base}permissions: [system.log_sources.create]\n").as_bytes(),
    )
    .unwrap_err();
    assert_eq!((e.line, e.column), (4, 15));
    assert_eq!(kind_of("binary: /system/a\n"), ErrorKind::Missing("name"));
    assert_eq!(kind_of("name: a\n"), ErrorKind::Missing("binary"));
    assert_eq!(
        kind_of("name: a\nbinary: /bin/a\nkind: normal\n"),
        ErrorKind::BadBinary
    );
    assert_eq!(
        kind_of("name: b\nbinary: /system/b\nkind: normal\n"),
        ErrorKind::FileName
    );
    let unprefixed = parse_driver("a", b"name: a\nbinary: /system/a\n").unwrap_err();
    assert_eq!(unprefixed.kind, ErrorKind::FileName);
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
        app_id: format!("system.driver.{name}"),
        boot: false,
        binary: format!("/system/drivers/system.driver.{name}/{name}"),
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

#[test]
fn refuses_services_and_programs_that_list_logs_add_source() {
    let listed = "permissions: [admin.logs.read, system.log_sources.create]\n";
    let service = parse("system.log", format!("{MINIMAL}{listed}").as_bytes()).unwrap_err();
    assert_eq!(
        (service.kind, service.line),
        (ErrorKind::ReservedPermission, 4)
    );
    let program = "app_id: system.console\nbinary: /system/bin/x\n";
    let program = parse_program(
        "system.console.yaml",
        format!("{program}{listed}").as_bytes(),
    );
    assert_eq!(program.unwrap_err().kind, ErrorKind::ReservedPermission);
    // Other permissions stay as listed.
    let read = manifest(
        "system.log",
        &format!("{MINIMAL}permissions: [admin.logs.read]\n"),
    );
    assert_eq!(read.service.permissions, ["admin.logs.read"]);
}

#[test]
fn reads_installed_drivers_with_their_package_ids() {
    let text = b"name: org.example.drv\nbinary: /system/drivers/org.example.drv/drv\nboot: true\n";
    let m = parse_driver("org.example.drv", text).unwrap();
    assert_eq!(m.driver.app_id, "org.example.drv");
    assert!(m.driver.boot);
    let system = parse_driver("system.driver.a", b"name: a\nbinary: /system/a\n").unwrap();
    assert_eq!(system.driver.app_id, "system.driver.a");
    assert!(!system.driver.boot);
    // Its folder is its name, which is a package ID that is not the
    // system's.
    let kind_of = |dir: &str, text: &[u8]| parse_driver(dir, text).unwrap_err().kind;
    assert_eq!(kind_of("org.example.other", text), ErrorKind::FileName);
    let bad = b"name: Org.Drv\nbinary: /system/drivers/Org.Drv/drv\n";
    assert_eq!(kind_of("Org.Drv", bad), ErrorKind::BadName);
    let system_id = b"name: system.drv\nbinary: /system/drivers/system.drv/drv\n";
    assert_eq!(kind_of("system.drv", system_id), ErrorKind::BadName);
}

#[test]
fn reads_services_of_packages() {
    let text = b"name: index\nbinary: /apps/org.example.editor/bin/index\nstart: on-demand\n\
                 permissions: [user.documents.access]\n";
    let m = parse_package_service("org.example.editor", "index", text).unwrap();
    assert_eq!(m.service.name, "org.example.editor.index");
    assert_eq!(m.service.app_id, "org.example.editor");
    let system = parse(
        "system.logsvc",
        b"name: logsvc\nbinary: /system/logsvc\nstart: boot\n",
    )
    .unwrap();
    assert_eq!(system.service.app_id, "system.logsvc");
    let kind_of = |key: &str, text: &[u8]| {
        parse_package_service("org.example.editor", key, text)
            .unwrap_err()
            .kind
    };
    assert_eq!(kind_of("other", text), ErrorKind::FileName);
    let outside = b"name: index\nbinary: /system/bin/index\nstart: boot\n";
    assert_eq!(kind_of("index", outside), ErrorKind::BadBinary);
    let admin = b"name: index\nbinary: /apps/org.example.editor/i\nstart: boot\npermissions: [admin.logs.read]\n";
    assert_eq!(kind_of("index", admin), ErrorKind::ReservedPermission);
    let handles = b"name: index\nbinary: /apps/org.example.editor/i\nstart: boot\nboot_handles: [root_budget]\n";
    assert_eq!(kind_of("index", handles), ErrorKind::BadBootHandle);
}
