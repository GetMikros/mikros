//! `cargo xtask build`: builds the system components.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The build kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Production,
    Debug,
    Fuzz,
}

impl Kind {
    /// The name of the kind on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Production => "production",
            Kind::Debug => "debug",
            Kind::Fuzz => "fuzz",
        }
    }

    /// The Cargo profile used for this kind.
    pub fn profile(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug => "dev",
            Kind::Fuzz => "fuzz",
        }
    }

    /// The directory Cargo writes the profile's output to.
    pub fn profile_dir(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug => "debug",
            Kind::Fuzz => "fuzz",
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// The build kind.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
}

/// The paths of the built components.
pub struct Built {
    pub bootmenu: PathBuf,
    pub bootloader: PathBuf,
    pub kernel: PathBuf,
    pub boot_image: PathBuf,
    /// Where xkbcommon was built.
    pub xkbcommon: crate::ports::Built,
}

/// The boot images that can be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootImage {
    /// The system's boot image.
    System,
    /// The boot image of the integration tests, whose init runs them.
    Test,
}

/// Builds every component of the build kind `kind`.
pub fn build(kind: Kind) -> Result<Built> {
    build_for(kind, BootImage::System)
}

/// Builds every component of the build kind `kind`, with `boot` as the
/// boot image.
pub fn build_for(kind: Kind, boot: BootImage) -> Result<Built> {
    let bootmenu = cargo_build("bootmenu", "bootmenu", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootmenu.display());
    let bootloader = cargo_build("mikrosboot", "mikrosboot", "x86_64-unknown-uefi", kind)?;
    println!("built    {}", bootloader.display());
    crate::loader_check::check(&bootloader)?;
    println!(
        "checked  {} holds no MP services protocol GUID",
        bootloader.display()
    );
    let kernel = cargo_build("kernel-x86_64", "kernel", KERNEL_TARGET, kind)?;
    println!("built    {}", kernel.display());
    crate::extended::check(&kernel)?;
    println!("checked  {} uses no x87 or vector state", kernel.display());
    if kind == Kind::Debug {
        crate::debug_build::check(&kernel)?;
        println!(
            "checked  {} has the debug build's settings",
            kernel.display()
        );
    }
    let debug_info = crate::debuginfo::split(&kernel)?;
    println!("built    {}", debug_info.display());
    if kind == Kind::Fuzz {
        let dir = syzkaller_descriptions()?;
        println!("built    {}", dir.display());
    }
    let xkbcommon = crate::ports::build(&crate::ports::XKBCOMMON)?;
    crate::build_systems::check_installed(
        &xkbcommon.install,
        &[
            "lib/libxkbcommon.0.13.2.dlib",
            "include/xkbcommon/xkbcommon.h",
            "bin/xkbcli",
        ],
        &[
            ("lib/libxkbcommon.0.dlib", "libxkbcommon.0.13.2.dlib"),
            ("lib/libxkbcommon.dlib", "libxkbcommon.0.dlib"),
        ],
    )?;
    println!("built    {}", xkbcommon.install.display());
    let boot_image = match boot {
        BootImage::System => {
            let init = user_build("init", kind)?;
            println!("built    {}", init.display());
            let manager = user_build("process-manager", kind)?;
            println!("built    {}", manager.display());
            let services = user_build("service-manager", kind)?;
            println!("built    {}", services.display());
            let broker = user_build("permissions-broker", kind)?;
            println!("built    {}", broker.display());
            let devices = user_build("device-manager", kind)?;
            println!("built    {}", devices.display());
            let pci_bus = user_build("pci-bus", kind)?;
            println!("built    {}", pci_bus.display());
            let acpi = acpi_build(kind)?;
            let acpi_manifest = crate::workspace_root().join("user/drivers/acpi/driver.yaml");
            let rtc = user_build("rtc", kind)?;
            println!("built    {}", rtc.display());
            let rtc_manifest = crate::workspace_root().join("user/drivers/rtc/driver.yaml");
            let virtio_blk = user_build("virtio-blk", kind)?;
            println!("built    {}", virtio_blk.display());
            let virtio_blk_manifest =
                crate::workspace_root().join("user/drivers/virtio-blk/driver.yaml");
            let nvme = user_build("nvme", kind)?;
            println!("built    {}", nvme.display());
            let nvme_manifest = crate::workspace_root().join("user/drivers/nvme/driver.yaml");
            let ahci = user_build("ahci", kind)?;
            println!("built    {}", ahci.display());
            let ahci_manifest = crate::workspace_root().join("user/drivers/ahci/driver.yaml");
            let xhci = user_build("xhci", kind)?;
            println!("built    {}", xhci.display());
            let xhci_manifest = crate::workspace_root().join("user/drivers/xhci/driver.yaml");
            let usb_storage = user_build("usb-storage", kind)?;
            println!("built    {}", usb_storage.display());
            let usb_storage_manifest =
                crate::workspace_root().join("user/drivers/usb-storage/driver.yaml");
            let partition = user_build("partition", kind)?;
            println!("built    {}", partition.display());
            let partition_manifest =
                crate::workspace_root().join("user/drivers/partition/driver.yaml");
            let fat = user_build("fat", kind)?;
            println!("built    {}", fat.display());
            let fat_manifest = crate::workspace_root().join("user/drivers/fat/driver.yaml");
            let exfat = user_build("exfat", kind)?;
            println!("built    {}", exfat.display());
            let exfat_manifest = crate::workspace_root().join("user/drivers/exfat/driver.yaml");
            let btrfs = btrfs_build(kind)?;
            let btrfs_manifest = crate::workspace_root().join("user/drivers/btrfs/driver.yaml");
            let tmpfs = user_build("tmpfs", kind)?;
            println!("built    {}", tmpfs.display());
            let tmpfs_manifest = crate::workspace_root().join("user/tmpfs/tmpfs.yaml");
            let union = user_build("union", kind)?;
            println!("built    {}", union.display());
            let union_manifest = crate::workspace_root().join("user/union/union.yaml");
            let log_service = user_build("log-service", kind)?;
            println!("built    {}", log_service.display());
            let log_manifest = crate::workspace_root().join("user/log-service/log.yaml");
            let time_service = user_build("time-service", kind)?;
            println!("built    {}", time_service.display());
            let time_manifest = crate::workspace_root().join("user/time-service/time.yaml");
            let broker_manifest =
                crate::workspace_root().join("user/permissions-broker/permissions.yaml");
            let devices_manifest =
                crate::workspace_root().join("user/device-manager/device-manager.yaml");
            let pci_bus_manifest = crate::workspace_root().join("user/drivers/pci-bus/driver.yaml");
            let members = [
                ("init", init.as_path()),
                ("usr/bin/process-manager", manager.as_path()),
                ("usr/bin/service-manager", services.as_path()),
                ("usr/bin/permissions-broker", broker.as_path()),
                (
                    "usr/lib/services/permissions.yaml",
                    broker_manifest.as_path(),
                ),
                ("usr/bin/tmpfs", tmpfs.as_path()),
                ("usr/lib/services/tmpfs.yaml", tmpfs_manifest.as_path()),
                ("usr/bin/union", union.as_path()),
                ("usr/lib/services/union.yaml", union_manifest.as_path()),
                ("usr/bin/log-service", log_service.as_path()),
                ("usr/lib/services/log.yaml", log_manifest.as_path()),
                ("usr/bin/time-service", time_service.as_path()),
                ("usr/lib/services/time.yaml", time_manifest.as_path()),
                ("usr/bin/device-manager", devices.as_path()),
                (
                    "usr/lib/services/device-manager.yaml",
                    devices_manifest.as_path(),
                ),
                ("usr/lib/drivers/pci-bus/pci-bus", pci_bus.as_path()),
                (
                    "usr/lib/drivers/pci-bus/driver.yaml",
                    pci_bus_manifest.as_path(),
                ),
                ("usr/lib/drivers/acpi/acpi", acpi.as_path()),
                ("usr/lib/drivers/acpi/driver.yaml", acpi_manifest.as_path()),
                ("usr/lib/drivers/rtc/rtc", rtc.as_path()),
                ("usr/lib/drivers/rtc/driver.yaml", rtc_manifest.as_path()),
                (
                    "usr/lib/drivers/virtio-blk/virtio-blk",
                    virtio_blk.as_path(),
                ),
                (
                    "usr/lib/drivers/virtio-blk/driver.yaml",
                    virtio_blk_manifest.as_path(),
                ),
                ("usr/lib/drivers/nvme/nvme", nvme.as_path()),
                ("usr/lib/drivers/nvme/driver.yaml", nvme_manifest.as_path()),
                ("usr/lib/drivers/ahci/ahci", ahci.as_path()),
                ("usr/lib/drivers/ahci/driver.yaml", ahci_manifest.as_path()),
                ("usr/lib/drivers/xhci/xhci", xhci.as_path()),
                ("usr/lib/drivers/xhci/driver.yaml", xhci_manifest.as_path()),
                (
                    "usr/lib/drivers/usb-storage/usb-storage",
                    usb_storage.as_path(),
                ),
                (
                    "usr/lib/drivers/usb-storage/driver.yaml",
                    usb_storage_manifest.as_path(),
                ),
                ("usr/lib/drivers/partition/partition", partition.as_path()),
                (
                    "usr/lib/drivers/partition/driver.yaml",
                    partition_manifest.as_path(),
                ),
                ("usr/lib/drivers/fat/fat", fat.as_path()),
                ("usr/lib/drivers/fat/driver.yaml", fat_manifest.as_path()),
                ("usr/lib/drivers/exfat/exfat", exfat.as_path()),
                (
                    "usr/lib/drivers/exfat/driver.yaml",
                    exfat_manifest.as_path(),
                ),
                ("usr/lib/drivers/btrfs/btrfs", btrfs.as_path()),
                (
                    "usr/lib/drivers/btrfs/driver.yaml",
                    btrfs_manifest.as_path(),
                ),
            ];
            // The time zones, which the root partition gets with the rest
            // of usr.
            let zones = crate::tzdata::members(&crate::tzdata::zoneinfo()?)?;
            let members: Vec<(&str, &Path)> = members
                .into_iter()
                .chain(zones.iter().map(|(at, path)| (at.as_str(), path.as_path())))
                .collect();
            boot_image(kind, "boot.img", &members)?
        }
        BootImage::Test => {
            let init = user_build("test-init", kind)?;
            println!("built    {}", init.display());
            let echo = user_build("channel-echo", kind)?;
            println!("built    {}", echo.display());
            let server = user_build("call-server", kind)?;
            println!("built    {}", server.display());
            let debuggee = user_build("debuggee", kind)?;
            println!("built    {}", debuggee.display());
            let pie = user_build("pie", kind)?;
            println!("built    {}", pie.display());
            let managed = user_build("managed", kind)?;
            println!("built    {}", managed.display());
            let manager = user_build("process-manager", kind)?;
            println!("built    {}", manager.display());
            let services = user_build("service-manager", kind)?;
            println!("built    {}", services.display());
            let probe = user_build("launcher-probe", kind)?;
            println!("built    {}", probe.display());
            let hello = c_build(
                "hello-c",
                &["user/tests/hello-c/hello.c"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", hello.display());
            let hello_dynamic = c_build(
                "hello-c-dynamic",
                &["user/tests/hello-c/hello.c"],
                Linking::Dynamic,
                &[],
            )?;
            println!("built    {}", hello_dynamic.display());
            let raw_syscall = c_build(
                "raw-syscall",
                &["user/tests/gate-c/raw-syscall.c"],
                Linking::Dynamic,
                &[],
            )?;
            println!("built    {}", raw_syscall.display());
            let spawn_child = c_build(
                "spawn-child",
                &["user/tests/spawn-c/child.c"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", spawn_child.display());
            let greet = c_build(
                "libgreet",
                &["user/tests/library-c/greet.c"],
                Linking::Shared("libgreet.1.dlib"),
                &[],
            )?;
            println!("built    {}", greet.display());
            let decoy = c_build(
                "libgreet-decoy",
                &["user/tests/library-c/decoy.c"],
                Linking::Shared("libgreet.1.dlib"),
                &[],
            )?;
            println!("built    {}", decoy.display());
            // Without the start files, the library has no ABI note.
            let no_note = c_build(
                "libnonote",
                &["user/tests/library-c/decoy.c"],
                Linking::Shared("libnonote.1.dlib"),
                &["-nostartfiles".to_owned()],
            )?;
            println!("built    {}", no_note.display());
            // The program names the decoy's directory in a DT_RPATH,
            // which the dynamic linker ignores.
            let library_user = c_build(
                "library-user",
                &["user/tests/library-c/main.c"],
                Linking::Dynamic,
                &[
                    format!("-L{}", c_dir("libgreet").display()),
                    "-lgreet".to_owned(),
                    "-Wl,--disable-new-dtags,-rpath,/usr/share/decoy".to_owned(),
                ],
            )?;
            println!("built    {}", library_user.display());
            // The same project built with CMake and with Meson, through
            // the sysroot's toolchain files.
            let tally_source = crate::workspace_root().join("user/tests/build-systems");
            let cmake_tree = crate::build_systems::cmake("tally-cmake", &tally_source, &[])?;
            let meson_tree = crate::build_systems::meson("tally-meson", &tally_source, &[])?;
            for (tree, system) in [(&cmake_tree, "cmake"), (&meson_tree, "meson")] {
                let real = format!("libtally-{system}.1.2.3.dlib");
                let major = format!("libtally-{system}.1.dlib");
                // The library without a version has the major version 0.
                let base = format!("libbase-{system}.0.dlib");
                crate::build_systems::check_installed(
                    tree,
                    &[
                        &format!("bin/tally-{system}-user"),
                        &format!("lib/{real}"),
                        &format!("lib/{base}"),
                        &format!("lib/libcount-{system}.slib"),
                    ],
                    &[
                        (&format!("lib/{major}"), &real),
                        (&format!("lib/libtally-{system}.dlib"), &major),
                        (&format!("lib/libbase-{system}.dlib"), &base),
                    ],
                )?;
                println!("built    {}", tree.display());
            }
            let hello_std = crate::rust_std::build("hello-std", kind)?;
            println!("built    {}", hello_std.display());
            let spawn_std = crate::rust_std::build("spawn-std", kind)?;
            println!("built    {}", spawn_std.display());
            let programs = crate::workspace_root().join("user/tests/launcher-probe/programs");
            let (prog, closed, bad) = (
                programs.join("system.prog.yaml"),
                programs.join("system.closed.yaml"),
                programs.join("system.bad.yaml"),
            );
            let dynamic_linker = crate::sysroot::dir().join("lib/ld-mikros.dlib");
            let greeting = crate::workspace_root().join("user/tests/hello-c/greeting.txt");
            // The process manager tests serve this image, in which the
            // probe takes the service manager's place.
            let launcher = archive(
                kind,
                "launcher.img",
                &[
                    ("usr/bin/service-manager", probe.as_path()),
                    ("usr/bin/probe", probe.as_path()),
                    ("usr/bin/hello", hello.as_path()),
                    ("usr/bin/hello-dynamic", hello_dynamic.as_path()),
                    ("usr/bin/raw-syscall", raw_syscall.as_path()),
                    // Applications that the boot image provides.
                    ("Apps/test/bin/hello", hello.as_path()),
                    ("Apps/test/bin/hello-dynamic", hello_dynamic.as_path()),
                    ("Apps/test/bin/raw-syscall", raw_syscall.as_path()),
                    // An app's bundle, and program manifests.
                    ("Apps/test.app/bin/hello", hello_dynamic.as_path()),
                    ("usr/lib/programs/system.prog.yaml", prog.as_path()),
                    ("usr/lib/programs/system.closed.yaml", closed.as_path()),
                    ("usr/lib/programs/system.bad.yaml", bad.as_path()),
                    ("usr/bin/hello-std", hello_std.as_path()),
                    ("usr/bin/spawn-std", spawn_std.as_path()),
                    ("usr/bin/spawn-child", spawn_child.as_path()),
                    ("usr/lib/ld-mikros.dlib", dynamic_linker.as_path()),
                    ("usr/bin/library-user", library_user.as_path()),
                    ("usr/lib/libgreet.1.dlib", greet.as_path()),
                    ("usr/share/decoy/libgreet.1.dlib", decoy.as_path()),
                    ("usr/lib/libnonote.1.dlib", no_note.as_path()),
                    (
                        "usr/bin/tally-cmake-user",
                        &cmake_tree.join("bin/tally-cmake-user"),
                    ),
                    (
                        "usr/lib/libtally-cmake.1.dlib",
                        &cmake_tree.join("lib/libtally-cmake.1.dlib"),
                    ),
                    (
                        "usr/lib/libbase-cmake.0.dlib",
                        &cmake_tree.join("lib/libbase-cmake.0.dlib"),
                    ),
                    (
                        "usr/bin/tally-meson-user",
                        &meson_tree.join("bin/tally-meson-user"),
                    ),
                    (
                        "usr/lib/libtally-meson.1.dlib",
                        &meson_tree.join("lib/libtally-meson.1.dlib"),
                    ),
                    (
                        "usr/lib/libbase-meson.0.dlib",
                        &meson_tree.join("lib/libbase-meson.0.dlib"),
                    ),
                    ("usr/share/hello/greeting.txt", greeting.as_path()),
                ],
                &[],
                &[
                    ("usr/share/hello/near", "greeting.txt"),
                    ("usr/share/hello/far", "/usr/share/hello/greeting.txt"),
                    ("usr/bin/app-hello", "../../Apps/test/bin/hello"),
                ],
            )?;
            println!("built    {}", launcher.display());
            let runner = user_build("port-tests", kind)?;
            println!("built    {}", runner.display());
            // The ported software test serves this image, in which the
            // runner of the ported projects' tests takes the service
            // manager's place.
            let plan = crate::workspace_root().join("target/ported/port-tests.plan");
            let mut members =
                crate::port_tests::members(&crate::ports::XKBCOMMON, &xkbcommon, &plan)?;
            members.push(("usr/bin/service-manager".into(), runner));
            members.push(("usr/lib/ld-mikros.dlib".into(), dynamic_linker.clone()));
            let members: Vec<(&str, &Path)> = members
                .iter()
                .map(|(n, p)| (n.as_str(), p.as_path()))
                .collect();
            let ports_image = test_image(kind, "ports.img", &members)?;
            println!("built    {}", ports_image.display());
            let broker = user_build("permissions-broker", kind)?;
            println!("built    {}", broker.display());
            let broker_probe = user_build("broker-probe", kind)?;
            println!("built    {}", broker_probe.display());
            // The broker tests serve this image, in which the broker
            // probe takes the service manager's place.
            let broker_image = test_image(
                kind,
                "broker.img",
                &[
                    ("usr/bin/service-manager", broker_probe.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                ],
            )?;
            println!("built    {}", broker_image.display());
            let directory_probe = user_build("directory-probe", kind)?;
            println!("built    {}", directory_probe.display());
            let directory_probe_manifest =
                crate::workspace_root().join("user/tests/directory-probe/directory-probe.yaml");
            let restart_probe_manifest =
                crate::workspace_root().join("user/tests/directory-probe/restart-probe.yaml");
            let demand_probe_manifest =
                crate::workspace_root().join("user/tests/directory-probe/demand-probe.yaml");
            let demand_dep_manifest =
                crate::workspace_root().join("user/tests/directory-probe/demand-dep.yaml");
            // The service manager tests serve this image, which holds the
            // system's services and their manifests, and a probe that
            // starts as a boot service.
            let broker_manifest =
                crate::workspace_root().join("user/permissions-broker/permissions.yaml");
            let services_image = test_image(
                kind,
                "services.img",
                &[
                    ("usr/bin/service-manager", services.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                    (
                        "usr/lib/services/permissions.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("usr/bin/directory-probe", directory_probe.as_path()),
                    (
                        "usr/lib/services/directory-probe.yaml",
                        directory_probe_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/restart-probe.yaml",
                        restart_probe_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/demand-probe.yaml",
                        demand_probe_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/demand-dep.yaml",
                        demand_dep_manifest.as_path(),
                    ),
                ],
            )?;
            println!("built    {}", services_image.display());
            // The temporary file system test serves this image: the
            // service manager, the broker, the temporary file system, the
            // union server and probes that drive them, with their
            // manifests.
            let tmpfs = user_build("tmpfs", kind)?;
            println!("built    {}", tmpfs.display());
            let tmpfs_probe = user_build("tmpfs-probe", kind)?;
            println!("built    {}", tmpfs_probe.display());
            let tmpfs_manifest = crate::workspace_root().join("user/tmpfs/tmpfs.yaml");
            let tmpfs_probe_manifest =
                crate::workspace_root().join("user/tests/tmpfs-probe/tmpfs-probe.yaml");
            let union = user_build("union", kind)?;
            println!("built    {}", union.display());
            let union_probe = user_build("union-probe", kind)?;
            println!("built    {}", union_probe.display());
            let union_manifest = crate::workspace_root().join("user/union/union.yaml");
            let union_probe_manifest =
                crate::workspace_root().join("user/tests/union-probe/union-probe.yaml");
            let tmpfs_image = test_image(
                kind,
                "tmpfs.img",
                &[
                    ("usr/bin/service-manager", services.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                    (
                        "usr/lib/services/permissions.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("usr/bin/tmpfs", tmpfs.as_path()),
                    ("usr/lib/services/tmpfs.yaml", tmpfs_manifest.as_path()),
                    ("usr/bin/tmpfs-probe", tmpfs_probe.as_path()),
                    (
                        "usr/lib/services/tmpfs-probe.yaml",
                        tmpfs_probe_manifest.as_path(),
                    ),
                    ("usr/bin/union", union.as_path()),
                    ("usr/lib/services/union.yaml", union_manifest.as_path()),
                    ("usr/bin/union-probe", union_probe.as_path()),
                    (
                        "usr/lib/services/union-probe.yaml",
                        union_probe_manifest.as_path(),
                    ),
                ],
            )?;
            println!("built    {}", tmpfs_image.display());
            // The log service test serves this image: the service manager,
            // the broker, the log service and a probe that writes and reads
            // the log, with their manifests.
            let log_service = user_build("log-service", kind)?;
            println!("built    {}", log_service.display());
            let log_manifest = crate::workspace_root().join("user/log-service/log.yaml");
            let log_probe = user_build("log-probe", kind)?;
            println!("built    {}", log_probe.display());
            let log_probe_manifest =
                crate::workspace_root().join("user/tests/log-probe/log-probe.yaml");
            let log_image = test_image(
                kind,
                "log.img",
                &[
                    ("usr/bin/service-manager", services.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                    (
                        "usr/lib/services/permissions.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("usr/bin/log-service", log_service.as_path()),
                    ("usr/lib/services/log.yaml", log_manifest.as_path()),
                    ("usr/bin/log-probe", log_probe.as_path()),
                    (
                        "usr/lib/services/log-probe.yaml",
                        log_probe_manifest.as_path(),
                    ),
                ],
            )?;
            println!("built    {}", log_image.display());
            // The time service test serves this image: the service manager,
            // the broker, the time service and the time probe as two
            // services, with their manifests.
            let time_service = user_build("time-service", kind)?;
            println!("built    {}", time_service.display());
            let time_manifest = crate::workspace_root().join("user/time-service/time.yaml");
            let time_probe = user_build("time-probe", kind)?;
            println!("built    {}", time_probe.display());
            let probe_dir = crate::workspace_root().join("user/tests/time-probe");
            let time_image = test_image(
                kind,
                "time.img",
                &[
                    ("usr/bin/service-manager", services.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                    (
                        "usr/lib/services/permissions.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("usr/bin/time-service", time_service.as_path()),
                    ("usr/lib/services/time.yaml", time_manifest.as_path()),
                    ("usr/bin/time-probe", time_probe.as_path()),
                    (
                        "usr/lib/services/time-probe.yaml",
                        probe_dir.join("time-probe.yaml").as_path(),
                    ),
                    (
                        "usr/lib/services/time-guest.yaml",
                        probe_dir.join("time-guest.yaml").as_path(),
                    ),
                ],
            )?;
            println!("built    {}", time_image.display());
            // The namespace test serves this image, with root=bootfs: the
            // service manager, the broker, the temporary file system and a
            // probe of its namespace, with their manifests, files in /etc,
            // /usr and /Libraries that the probe reads, and a native app's
            // bundle with the dynamic linker it needs.
            let namespace_probe = user_build("namespace-probe", kind)?;
            println!("built    {}", namespace_probe.display());
            let probe_dir = crate::workspace_root().join("user/tests/namespace-probe");
            let namespace_probe_manifest = probe_dir.join("namespace-probe.yaml");
            let probe_conf = probe_dir.join("probe.conf");
            let probe_data = probe_dir.join("data.txt");
            let namespaces_image = test_image(
                kind,
                "namespaces.img",
                &[
                    ("usr/bin/service-manager", services.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                    (
                        "usr/lib/services/permissions.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("usr/bin/tmpfs", tmpfs.as_path()),
                    ("usr/lib/services/tmpfs.yaml", tmpfs_manifest.as_path()),
                    ("usr/bin/namespace-probe", namespace_probe.as_path()),
                    (
                        "usr/lib/services/namespace-probe.yaml",
                        namespace_probe_manifest.as_path(),
                    ),
                    ("usr/share/probe/data.txt", probe_data.as_path()),
                    ("etc/probe.conf", probe_conf.as_path()),
                    ("Libraries/probe.txt", probe_data.as_path()),
                    ("Apps/test.native/bin/hello", hello_dynamic.as_path()),
                    ("usr/lib/ld-mikros.dlib", dynamic_linker.as_path()),
                ],
            )?;
            println!("built    {}", namespaces_image.display());
            // The accounts test serves this image, with root=bootfs: the
            // service manager, the broker, the temporary file system and
            // the accounts service, with their manifests, the probe's
            // binary with the manifests of its five services, and a
            // users file in /var/lib/accounts.
            let accounts = user_build("accounts", kind)?;
            println!("built    {}", accounts.display());
            let accounts_probe = user_build("accounts-probe", kind)?;
            println!("built    {}", accounts_probe.display());
            let accounts_manifest = crate::workspace_root().join("user/accounts/accounts.yaml");
            let probe_dir = crate::workspace_root().join("user/tests/accounts-probe");
            let accounts_probe_manifest = probe_dir.join("accounts-probe.yaml");
            let accounts_guest_manifest = probe_dir.join("accounts-guest.yaml");
            let stub_manifest = probe_dir.join("authorization-stub.yaml");
            let thief_manifest = probe_dir.join("accounts-thief.yaml");
            let sneak_manifest = probe_dir.join("accounts-sneak.yaml");
            let users = accounts_users(kind)?;
            let accounts_image = test_image(
                kind,
                "accounts.img",
                &[
                    ("usr/bin/service-manager", services.as_path()),
                    ("usr/bin/permissions-broker", broker.as_path()),
                    (
                        "usr/lib/services/permissions.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("usr/bin/tmpfs", tmpfs.as_path()),
                    ("usr/lib/services/tmpfs.yaml", tmpfs_manifest.as_path()),
                    ("usr/bin/accounts", accounts.as_path()),
                    (
                        "usr/lib/services/accounts.yaml",
                        accounts_manifest.as_path(),
                    ),
                    ("usr/bin/accounts-probe", accounts_probe.as_path()),
                    (
                        "usr/lib/services/accounts-probe.yaml",
                        accounts_probe_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/accounts-guest.yaml",
                        accounts_guest_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/authorization-stub.yaml",
                        stub_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/accounts-thief.yaml",
                        thief_manifest.as_path(),
                    ),
                    (
                        "usr/lib/services/accounts-sneak.yaml",
                        sneak_manifest.as_path(),
                    ),
                    ("var/lib/accounts/users", users.as_path()),
                ],
            )?;
            println!("built    {}", accounts_image.display());
            // The device manager tests serve these images. Each holds the
            // service manager, the broker and the device manager with
            // their manifests, and test drivers: their manifests from
            // user/tests/drivers/, and the test driver's binary for those
            // that start.
            let devices = user_build("device-manager", kind)?;
            println!("built    {}", devices.display());
            let test_driver = user_build("test-driver", kind)?;
            println!("built    {}", test_driver.display());
            let pci_bus = user_build("pci-bus", kind)?;
            println!("built    {}", pci_bus.display());
            let virtio_blk = user_build("virtio-blk", kind)?;
            println!("built    {}", virtio_blk.display());
            let nvme = user_build("nvme", kind)?;
            println!("built    {}", nvme.display());
            let ahci = user_build("ahci", kind)?;
            println!("built    {}", ahci.display());
            let xhci = user_build("xhci", kind)?;
            println!("built    {}", xhci.display());
            let usb_storage = user_build("usb-storage", kind)?;
            println!("built    {}", usb_storage.display());
            let partition = user_build("partition", kind)?;
            println!("built    {}", partition.display());
            let fat = user_build("fat", kind)?;
            println!("built    {}", fat.display());
            let exfat = user_build("exfat", kind)?;
            println!("built    {}", exfat.display());
            let btrfs = btrfs_build(kind)?;
            let acpi = acpi_build(kind)?;
            let rtc = user_build("rtc", kind)?;
            println!("built    {}", rtc.display());
            let power_probe = user_build("power-probe", kind)?;
            println!("built    {}", power_probe.display());
            let devices_manifest =
                crate::workspace_root().join("user/device-manager/device-manager.yaml");
            let device_image = |file: &str,
                                drivers: &[(&str, bool)],
                                more: &[(String, PathBuf)]|
             -> Result<PathBuf> {
                let mut members: Vec<(String, PathBuf)> = vec![
                    ("usr/bin/service-manager".into(), services.clone()),
                    ("usr/bin/permissions-broker".into(), broker.clone()),
                    (
                        "usr/lib/services/permissions.yaml".into(),
                        broker_manifest.clone(),
                    ),
                    ("usr/bin/device-manager".into(), devices.clone()),
                    (
                        "usr/lib/services/device-manager.yaml".into(),
                        devices_manifest.clone(),
                    ),
                ];
                for &(name, starts) in drivers {
                    let manifest = format!("user/tests/drivers/{name}/driver.yaml");
                    members.push((
                        format!("usr/lib/drivers/{name}/driver.yaml"),
                        crate::workspace_root().join(manifest),
                    ));
                    if starts {
                        let binary = format!("usr/lib/drivers/{name}/{name}");
                        members.push((binary, test_driver.clone()));
                    }
                }
                members.extend_from_slice(more);
                let members: Vec<(&str, &Path)> = members
                    .iter()
                    .map(|(name, path)| (name.as_str(), path.as_path()))
                    .collect();
                test_image(kind, file, &members)
            };
            let drivers = [
                ("test-host", true),
                ("test-child", true),
                ("test-fallback", false),
                ("test-invalid", false),
            ];
            let devices_image = device_image("devices.img", &drivers, &[])?;
            println!("built    {}", devices_image.display());
            let failing_image = device_image(
                "failing.img",
                &[("test-decline", true), ("test-close", true)],
                &[],
            )?;
            println!("built    {}", failing_image.display());
            // test-kept-child runs test-crash-host's copy of the test
            // driver, which keeps one copy of it in memory, not two.
            let restart_image = device_image(
                "restart.img",
                &[("test-crash-host", true), ("test-kept-child", false)],
                &[],
            )?;
            println!("built    {}", restart_image.display());
            // The ACPI server test serves this image, with the ACPI server
            // for the platform:acpi device, the RTC driver for the clock it
            // adds, the time service, which takes UTC from the clock, and a
            // test driver for the host bridge that checks the server's and
            // the clock's services as their client.
            let acpi_image = device_image(
                "acpi.img",
                &[("test-acpi", true)],
                &[
                    ("usr/lib/drivers/acpi/acpi".to_owned(), acpi.clone()),
                    (
                        "usr/lib/drivers/acpi/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/acpi/driver.yaml"),
                    ),
                    // The log service, through which the drivers' messages
                    // reach the kernel log.
                    ("usr/bin/log-service".to_owned(), log_service.clone()),
                    ("usr/lib/services/log.yaml".to_owned(), log_manifest.clone()),
                    ("usr/lib/drivers/rtc/rtc".to_owned(), rtc.clone()),
                    (
                        "usr/lib/drivers/rtc/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/rtc/driver.yaml"),
                    ),
                    ("usr/bin/time-service".to_owned(), time_service.clone()),
                    (
                        "usr/lib/services/time.yaml".to_owned(),
                        time_manifest.clone(),
                    ),
                ],
            )?;
            println!("built    {}", acpi_image.display());
            // The ACPI restart run serves this image: the ACPI server, the
            // clock's driver, the log and time services, and the restart
            // probe, which kills the ACPI server.
            let restart_probe = user_build("restart-probe", kind)?;
            println!("built    {}", restart_probe.display());
            let acpi_restart_image = device_image(
                "acpi-restart.img",
                &[],
                &[
                    ("usr/lib/drivers/acpi/acpi".to_owned(), acpi.clone()),
                    (
                        "usr/lib/drivers/acpi/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/acpi/driver.yaml"),
                    ),
                    ("usr/bin/log-service".to_owned(), log_service.clone()),
                    ("usr/lib/services/log.yaml".to_owned(), log_manifest.clone()),
                    ("usr/lib/drivers/rtc/rtc".to_owned(), rtc.clone()),
                    (
                        "usr/lib/drivers/rtc/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/rtc/driver.yaml"),
                    ),
                    ("usr/bin/time-service".to_owned(), time_service.clone()),
                    (
                        "usr/lib/services/time.yaml".to_owned(),
                        time_manifest.clone(),
                    ),
                    ("usr/bin/restart-probe".to_owned(), restart_probe.clone()),
                    (
                        "usr/lib/services/acpi-restart-probe.yaml".to_owned(),
                        crate::workspace_root()
                            .join("user/tests/restart-probe/acpi-restart-probe.yaml"),
                    ),
                ],
            )?;
            println!("built    {}", acpi_restart_image.display());
            // The power-off run serves this image, with the ACPI server, the
            // log service and the power probe, a boot service that the
            // service manager stops when the power button shuts the
            // machine down, and the PCI bus driver with the drivers of the
            // disk controllers, of the xHCI controller, of USB mass storage
            // and of partitions, whose devices the device manager removes
            // when it stops.
            let mut power_files: Vec<(String, PathBuf)> = vec![
                ("usr/lib/drivers/acpi/acpi".to_owned(), acpi.clone()),
                (
                    "usr/lib/drivers/acpi/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/acpi/driver.yaml"),
                ),
            ];
            for (name, binary) in [
                ("pci-bus", &pci_bus),
                ("virtio-blk", &virtio_blk),
                ("nvme", &nvme),
                ("ahci", &ahci),
                ("xhci", &xhci),
                ("usb-storage", &usb_storage),
                ("partition", &partition),
            ] {
                let manifest = format!("user/drivers/{name}/driver.yaml");
                power_files.extend([
                    (format!("usr/lib/drivers/{name}/{name}"), binary.clone()),
                    (
                        format!("usr/lib/drivers/{name}/driver.yaml"),
                        crate::workspace_root().join(manifest),
                    ),
                ]);
            }
            power_files.extend([
                // The log service, through which the server's messages
                // reach the kernel log.
                ("usr/bin/log-service".to_owned(), log_service.clone()),
                ("usr/lib/services/log.yaml".to_owned(), log_manifest.clone()),
                ("usr/bin/power-probe".to_owned(), power_probe.clone()),
                (
                    "usr/lib/services/power-probe.yaml".to_owned(),
                    crate::workspace_root().join("user/tests/power-probe/power-probe.yaml"),
                ),
            ]);
            let power_image = device_image("power.img", &[], &power_files)?;
            println!("built    {}", power_image.display());
            // The PCI bus driver tests serve this image, with the bus
            // driver and test drivers for the host bridge's function, for
            // QEMU's edu device and for the e1000e function, the
            // storage drivers, the xHCI driver and the USB mass storage
            // driver, with the partition driver, the FAT server for
            // the EFI system partition, a test driver for the root partition
            // that checks it, and one for each scratch partition.
            let pci_bus_files = [
                (
                    "usr/lib/drivers/pci-bus/pci-bus".to_owned(),
                    pci_bus.clone(),
                ),
                (
                    "usr/lib/drivers/pci-bus/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/pci-bus/driver.yaml"),
                ),
                (
                    "usr/lib/drivers/virtio-blk/virtio-blk".to_owned(),
                    virtio_blk.clone(),
                ),
                (
                    "usr/lib/drivers/virtio-blk/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/virtio-blk/driver.yaml"),
                ),
                ("usr/lib/drivers/nvme/nvme".to_owned(), nvme.clone()),
                (
                    "usr/lib/drivers/nvme/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/nvme/driver.yaml"),
                ),
                ("usr/lib/drivers/ahci/ahci".to_owned(), ahci.clone()),
                (
                    "usr/lib/drivers/ahci/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/ahci/driver.yaml"),
                ),
                ("usr/lib/drivers/xhci/xhci".to_owned(), xhci.clone()),
                (
                    "usr/lib/drivers/xhci/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/xhci/driver.yaml"),
                ),
                (
                    "usr/lib/drivers/usb-storage/usb-storage".to_owned(),
                    usb_storage.clone(),
                ),
                (
                    "usr/lib/drivers/usb-storage/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/usb-storage/driver.yaml"),
                ),
                (
                    "usr/lib/drivers/partition/partition".to_owned(),
                    partition.clone(),
                ),
                (
                    "usr/lib/drivers/partition/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/partition/driver.yaml"),
                ),
                ("usr/lib/drivers/fat/fat".to_owned(), fat.clone()),
                (
                    "usr/lib/drivers/fat/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/fat/driver.yaml"),
                ),
                ("usr/lib/drivers/exfat/exfat".to_owned(), exfat.clone()),
                (
                    "usr/lib/drivers/exfat/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/exfat/driver.yaml"),
                ),
                ("usr/lib/drivers/btrfs/btrfs".to_owned(), btrfs.clone()),
                (
                    "usr/lib/drivers/btrfs/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/btrfs/driver.yaml"),
                ),
                // The log service, which keeps the log in files on the
                // root file system and the EFI system partition.
                ("usr/bin/log-service".to_owned(), log_service.clone()),
                ("usr/lib/services/log.yaml".to_owned(), log_manifest.clone()),
                // The time service, which takes the time zone from the
                // root file system.
                ("usr/bin/time-service".to_owned(), time_service.clone()),
                (
                    "usr/lib/services/time.yaml".to_owned(),
                    time_manifest.clone(),
                ),
            ];
            // The time zones, for the FAT and exFAT servers' local times.
            let mut pci_bus_files = pci_bus_files.to_vec();
            pci_bus_files.extend(crate::tzdata::members(&crate::tzdata::zoneinfo()?)?);
            let pci_image = device_image(
                "pci.img",
                &[
                    ("test-pci", true),
                    ("test-edu", true),
                    ("test-small-bar", true),
                    ("test-reset", true),
                    ("test-disk", true),
                    ("test-fat", true),
                    ("test-btrfs", true),
                ],
                &pci_bus_files,
            )?;
            println!("built    {}", pci_image.display());
            // The USB volumes run serves this image, with root=bootfs: the
            // PCI bus, xHCI, USB mass storage, partition, FAT, exFAT and
            // btrfs drivers, the time zones and the log service, through
            // which the drivers' messages reach the serial output, from the
            // PCI test's files, the temporary file system, and the volumes
            // probe.
            let volumes_probe = user_build("volumes-probe", kind)?;
            println!("built    {}", volumes_probe.display());
            let volume_drivers = [
                "pci-bus",
                "xhci",
                "usb-storage",
                "partition",
                "fat",
                "exfat",
                "btrfs",
            ];
            let mut volumes_files: Vec<(String, PathBuf)> = pci_bus_files
                .iter()
                .filter(|(at, _)| {
                    volume_drivers
                        .iter()
                        .any(|d| at.starts_with(&format!("usr/lib/drivers/{d}/")))
                        || at.starts_with("usr/share/zoneinfo/")
                        || at.contains("log-service")
                        || at.ends_with("services/log.yaml")
                })
                .cloned()
                .collect();
            volumes_files.extend([
                ("usr/bin/tmpfs".to_owned(), tmpfs.clone()),
                (
                    "usr/lib/services/tmpfs.yaml".to_owned(),
                    tmpfs_manifest.clone(),
                ),
                ("usr/bin/volumes-probe".to_owned(), volumes_probe.clone()),
                (
                    "usr/lib/services/volumes-probe.yaml".to_owned(),
                    crate::workspace_root().join("user/tests/volumes-probe/volumes-probe.yaml"),
                ),
            ]);
            let volumes_image = device_image("volumes.img", &[], &volumes_files)?;
            println!("built    {}", volumes_image.display());
            // The root disk runs serve this image: the PCI bus driver, the
            // drivers of the disk controllers, the partition, FAT and btrfs
            // drivers, the time zones and the log service, from the PCI
            // test's files, without test drivers.
            let root_drivers = [
                "pci-bus",
                "virtio-blk",
                "nvme",
                "ahci",
                "partition",
                "fat",
                "btrfs",
            ];
            let root_files: Vec<(String, PathBuf)> = pci_bus_files
                .iter()
                .filter(|(at, _)| {
                    root_drivers
                        .iter()
                        .any(|d| at.starts_with(&format!("usr/lib/drivers/{d}/")))
                        || at.starts_with("usr/share/zoneinfo/")
                        || at.contains("log-service")
                        || at.ends_with("services/log.yaml")
                })
                .cloned()
                .collect();
            let root_image = device_image("root.img", &[], &root_files)?;
            println!("built    {}", root_image.display());
            // The bus restart runs serve this image: the root disk runs'
            // with the restart probe.
            let mut bus_files = root_files.clone();
            bus_files.extend([
                ("usr/bin/restart-probe".to_owned(), restart_probe.clone()),
                (
                    "usr/lib/services/restart-probe.yaml".to_owned(),
                    crate::workspace_root().join("user/tests/restart-probe/restart-probe.yaml"),
                ),
            ]);
            let bus_image = device_image("bus.img", &[], &bus_files)?;
            println!("built    {}", bus_image.display());
            // The durability run serves this image: the PCI bus driver, the
            // drivers of the disk controllers, of USB mass storage and of
            // partitions, and the log service, from the PCI test's files,
            // with the durability test driver.
            let durable_drivers = [
                "pci-bus",
                "virtio-blk",
                "nvme",
                "ahci",
                "xhci",
                "usb-storage",
                "partition",
            ];
            let durable_files: Vec<(String, PathBuf)> = pci_bus_files
                .iter()
                .filter(|(at, _)| {
                    durable_drivers
                        .iter()
                        .any(|d| at.starts_with(&format!("usr/lib/drivers/{d}/")))
                        || at.contains("log-service")
                        || at.ends_with("services/log.yaml")
                })
                .cloned()
                .collect();
            let durable_image =
                device_image("durable.img", &[("test-durable", true)], &durable_files)?;
            println!("built    {}", durable_image.display());
            let members = [
                ("tests/accounts.img", accounts_image.as_path()),
                ("tests/broker.img", broker_image.as_path()),
                ("tests/services.img", services_image.as_path()),
                ("tests/tmpfs.img", tmpfs_image.as_path()),
                ("tests/log.img", log_image.as_path()),
                ("tests/time.img", time_image.as_path()),
                ("tests/namespaces.img", namespaces_image.as_path()),
                ("tests/devices.img", devices_image.as_path()),
                ("tests/failing.img", failing_image.as_path()),
                ("tests/restart.img", restart_image.as_path()),
                ("tests/acpi.img", acpi_image.as_path()),
                ("tests/power.img", power_image.as_path()),
                ("tests/pci.img", pci_image.as_path()),
                ("tests/volumes.img", volumes_image.as_path()),
                ("tests/root.img", root_image.as_path()),
                ("tests/bus.img", bus_image.as_path()),
                ("tests/durable.img", durable_image.as_path()),
                ("tests/acpi-restart.img", acpi_restart_image.as_path()),
                ("init", init.as_path()),
                ("tests/launcher.img", launcher.as_path()),
                ("tests/ports.img", ports_image.as_path()),
                ("tests/channel-echo", echo.as_path()),
                ("tests/call-server", server.as_path()),
                ("tests/debuggee", debuggee.as_path()),
                ("tests/pie", pie.as_path()),
                ("tests/managed", managed.as_path()),
                ("usr/bin/process-manager", manager.as_path()),
                ("usr/bin/service-manager", services.as_path()),
                // A dynamically linked program with its libraries and the
                // dynamic linker, which the test machine's root partition
                // gets with the rest of usr, and the nested systems' boot
                // images lack: the bus restart runs start it from the root.
                ("usr/bin/library-user", library_user.as_path()),
                ("usr/lib/libgreet.1.dlib", greet.as_path()),
                ("usr/lib/libnonote.1.dlib", no_note.as_path()),
                ("usr/lib/ld-mikros.dlib", dynamic_linker.as_path()),
            ];
            // The time zones, which the test machine's root partition
            // gets with the rest of usr.
            let zones = crate::tzdata::members(&crate::tzdata::zoneinfo()?)?;
            let members: Vec<(&str, &Path)> = members
                .into_iter()
                .chain(zones.iter().map(|(at, path)| (at.as_str(), path.as_path())))
                .collect();
            boot_image(kind, "test-boot.img", &members)?
        }
    };
    println!("built    {}", boot_image.display());
    Ok(Built {
        bootmenu,
        bootloader,
        kernel,
        boot_image,
        xkbcommon,
    })
}

/// Builds the boot image of a live system for `kind` from the system's
/// boot image `boot_image`: each of its members, `init` and the whole
/// system at their paths in the namespaces, and then the directories of
/// the root file system at their paths there.
pub fn live_boot_image(kind: Kind, boot_image: &Path) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join("live-boot.img");
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut live = tar::Builder::new(file);
    let source =
        File::open(boot_image).with_context(|| format!("opening {}", boot_image.display()))?;
    let mut source = tar::Archive::new(source);
    let entries = source
        .entries()
        .with_context(|| format!("reading {}", boot_image.display()))?;
    for entry in entries {
        let mut entry = entry.with_context(|| format!("reading {}", boot_image.display()))?;
        let header = entry.header().clone();
        live.append(&header, &mut entry)
            .context("adding a member to the live boot archive")?;
    }
    for dir in crate::image::live_dirs() {
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        live.append_data(&mut header, dir_name(&dir), std::io::empty())
            .with_context(|| format!("adding {dir} to the live boot archive"))?;
    }
    live.finish().context("finishing the live boot archive")?;
    Ok(path)
}

/// Writes the users file of the accounts test for `kind` and returns its
/// path: `ann`, an administrator, and `bob` and `cy`, standard users,
/// each with the password `<name>-password`. Fixed salts keep the file
/// the same from one build to the next.
fn accounts_users(kind: Kind) -> Result<PathBuf> {
    use mikros_userdb::{Kind as UserKind, Users, encode, hash};
    let mut users = Users::new();
    let people = [
        ("ann", "Ann", UserKind::Admin),
        ("bob", "Bob", UserKind::Standard),
        ("cy", "Cy", UserKind::Standard),
    ];
    for (i, (name, full_name, user_kind)) in people.into_iter().enumerate() {
        let salt = [i as u8 + 1; hash::SALT_LEN];
        let password = format!("{name}-password");
        let phc = hash::hash(password.as_bytes(), &salt).context("hashing a password")?;
        users
            .create(name, full_name, user_kind, phc)
            .map_err(|r| anyhow::anyhow!("adding {name}: {r:?}"))?;
    }
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join("accounts-users");
    fs::write(&path, encode(&users)).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Writes the boot archive `file` with each of `members`, given by name
/// and the path of its contents, as a regular file.
fn boot_image(kind: Kind, file: &str, members: &[(&str, &Path)]) -> Result<PathBuf> {
    archive(kind, file, members, &[], &[])
}

/// Like `boot_image`, for the images that the test boot image holds:
/// their programs go without their debug information, which takes most
/// of a debug build's bytes and which every image would otherwise hold a
/// copy of. Their symbols stay.
fn test_image(kind: Kind, file: &str, members: &[(&str, &Path)]) -> Result<PathBuf> {
    let stripped: Vec<(&str, PathBuf)> = members
        .iter()
        .map(|&(name, source)| Ok((name, without_debug_info(kind, source)?)))
        .collect::<Result<_>>()?;
    let members: Vec<(&str, &Path)> = stripped
        .iter()
        .map(|(name, path)| (*name, path.as_path()))
        .collect();
    boot_image(kind, file, &members)
}

/// A copy of the ELF file `source` without its debug information, made
/// again only when `source` is newer, or `source` itself when it is not
/// an ELF file.
fn without_debug_info(kind: Kind, source: &Path) -> Result<PathBuf> {
    let mut magic = [0u8; 4];
    let is_elf = File::open(source)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .is_ok()
        && magic == *b"\x7fELF";
    if !is_elf {
        return Ok(source.to_path_buf());
    }
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name())
        .join("stripped");
    fs::create_dir_all(&dir)?;
    // Programs of the same name come from one build directory.
    let name = source
        .strip_prefix(crate::workspace_root())
        .unwrap_or(source)
        .to_string_lossy()
        .replace('/', "_");
    let stripped = dir.join(name);
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    if modified(&stripped).is_some_and(|s| Some(s) >= modified(source)) {
        return Ok(stripped);
    }
    let mut objcopy = Command::new(crate::llvm::tool("llvm-objcopy"));
    objcopy.arg("--strip-debug").arg(source).arg(&stripped);
    let status = objcopy
        .status()
        .with_context(|| format!("stripping {}", source.display()))?;
    if !status.success() {
        bail!("stripping {} failed", source.display());
    }
    Ok(stripped)
}

/// Like `boot_image`, with the directories `dirs` after the files, and
/// the symbolic links `links`, given by name and target, after those.
fn archive(
    kind: Kind,
    file: &str,
    members: &[(&str, &Path)],
    dirs: &[&str],
    links: &[(&str, &str)],
) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join(file);
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut archive = tar::Builder::new(file);
    for &(name, source) in members {
        let data = fs::read(source).with_context(|| format!("reading {}", source.display()))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        archive
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("adding {name} to the boot archive"))?;
    }
    for &dir in dirs {
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        archive
            .append_data(&mut header, dir_name(dir), std::io::empty())
            .with_context(|| format!("adding {dir} to the boot archive"))?;
    }
    for &(name, target) in links {
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(tar::EntryType::Symlink);
        archive
            .append_link(&mut header, name, target)
            .with_context(|| format!("adding {name} to the boot archive"))?;
    }
    archive.finish().context("finishing the boot archive")?;
    Ok(path)
}

/// The archive name of directory `dir`, which ends in a slash.
fn dir_name(dir: &str) -> String {
    format!("{dir}/")
}

pub fn run(args: &Args) -> Result<()> {
    build(args.kind).map(drop)
}

/// The target the kernel is built for.
pub const KERNEL_TARGET: &str = "x86_64-unknown-none";

/// The target user-space programs are built for.
pub const USER_TARGET: &str = "targets/x86_64-unknown-mikros.json";

/// Builds the btrfs server, after the library of Linux's btrfs code on
/// the shim for Mikros that it links, and returns its path.
fn btrfs_build(kind: Kind) -> Result<PathBuf> {
    crate::btrfs::build(crate::btrfs::Flavor::Mikros)?;
    let btrfs = user_build("btrfs", kind)?;
    println!("built    {}", btrfs.display());
    Ok(btrfs)
}

/// Builds the ACPI server, after the library of ACPICA and its glue that
/// it links, and returns its path.
fn acpi_build(kind: Kind) -> Result<PathBuf> {
    crate::acpica::build(crate::acpica::Flavor::Mikros)?;
    let acpi = user_build("acpi", kind)?;
    println!("built    {}", acpi.display());
    Ok(acpi)
}

/// Builds the user-space program `bin`, a package of the `user`
/// workspace of the same name, and returns its path.
fn user_build(bin: &str, kind: Kind) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    // The user workspace has its own configuration, which Cargo reads
    // from the directory it runs in.
    let mut command = Command::new(cargo);
    command
        .current_dir(root.join("user"))
        .args(["build", "--package", bin, "--bin", bin, "--target"])
        .arg(root.join(USER_TARGET))
        .args(["--profile", kind.profile()]);
    // The test init of a fuzz build also tests the fuzz-only calls.
    if matches!(kind, Kind::Fuzz) && bin == "test-init" {
        command.args(["--features", "fuzz"]);
    }
    let status = command.status().context("failed to run cargo")?;
    if !status.success() {
        bail!("building {bin} failed");
    }
    Ok(root
        .join("user/target/x86_64-unknown-mikros")
        .join(kind.profile_dir())
        .join(bin))
}

/// How a C program is linked with the C library.
#[derive(Clone, Copy, PartialEq)]
enum Linking {
    Static,
    Dynamic,
    /// A shared library `<name>.dlib` with this `DT_SONAME`.
    Shared(&'static str),
}

/// Builds the C program or library `name` from `sources`, paths relative
/// to the repository, against the sysroot, with the further arguments
/// `args`, and returns its path, in `target/c/<name>/`.
fn c_build(name: &str, sources: &[&str], linking: Linking, args: &[String]) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let sysroot = crate::sysroot::build()?;
    let dir = c_dir(name);
    fs::create_dir_all(&dir)?;
    let mut command = Command::new(crate::llvm::tool("clang"));
    command.arg(format!(
        "--config={}",
        crate::sysroot::config(&sysroot).display()
    ));
    command.arg("-O2");
    let program = match linking {
        Linking::Static => {
            command.arg("-static-pie");
            dir.join(name)
        }
        Linking::Dynamic => dir.join(name),
        Linking::Shared(soname) => {
            command.args(["-shared", "-fPIC", &format!("-Wl,-soname,{soname}")]);
            dir.join(format!("{name}.dlib"))
        }
    };
    let status = command
        .arg("-o")
        .arg(&program)
        .args(sources.iter().map(|s| root.join(s)))
        .args(args)
        .status()
        .context("failed to run clang")?;
    if !status.success() {
        bail!("building {name} failed");
    }
    Ok(program)
}

/// The directory that `c_build` builds `name` in.
fn c_dir(name: &str) -> PathBuf {
    crate::workspace_root().join("target/c").join(name)
}

/// Writes the syzkaller descriptions and constants generated from the
/// system call table to `target/syzkaller/mikros`, and returns that
/// directory.
fn syzkaller_descriptions() -> Result<PathBuf> {
    let root = crate::workspace_root();
    let path = root.join("kernel/syscalls.yaml");
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let read = syscall_table::table::read(&bytes)
        .map_err(|e| anyhow::anyhow!("kernel/syscalls.yaml:{e}"))?;
    let dir = root.join("target/syzkaller/mikros");
    fs::create_dir_all(&dir)?;
    let table = &read.table;
    fs::write(
        dir.join("sys.txt"),
        syscall_table::syzkaller::descriptions(table, "x86_64"),
    )?;
    fs::write(
        dir.join("sys_amd64.const"),
        syscall_table::syzkaller::constants(table, "x86_64"),
    )?;
    Ok(dir)
}

/// The LLVM options that make every basic block call
/// `__sanitizer_cov_trace_pc`, as Linux's kcov expects.
const COVERAGE_FLAGS: [&str; 3] = [
    "-Cpasses=sancov-module",
    "-Cllvm-args=-sanitizer-coverage-level=3",
    "-Cllvm-args=-sanitizer-coverage-trace-pc",
];

/// The extra Cargo arguments for building `package` as `kind`: the fuzz
/// build of the kernel gets its fuzz feature and coverage
/// instrumentation, added to the kernel target's flags.
fn kernel_fuzz_args(package: &str, kind: Kind) -> Vec<String> {
    if package != "kernel-x86_64" || kind != Kind::Fuzz {
        return Vec::new();
    }
    let flags: Vec<String> = COVERAGE_FLAGS.iter().map(|f| format!("\"{f}\"")).collect();
    vec![
        "--features".into(),
        "fuzz".into(),
        "--config".into(),
        format!("target.{KERNEL_TARGET}.rustflags=[{}]", flags.join(", ")),
    ]
}

/// Builds the binary `bin` of `package` for `target` and returns its path.
fn cargo_build(package: &str, bin: &str, target: &str, kind: Kind) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(crate::workspace_root())
        .args([
            "build",
            "--package",
            package,
            "--bin",
            bin,
            "--target",
            target,
        ])
        .args(["--profile", kind.profile()])
        .args(kernel_fuzz_args(package, kind))
        .status()
        .context("failed to run cargo")?;
    if !status.success() {
        bail!("building {package} failed");
    }
    let extension = if target.ends_with("-uefi") { "efi" } else { "" };
    let mut path = crate::workspace_root()
        .join("target")
        .join(target)
        .join(kind.profile_dir())
        .join(bin);
    path.set_extension(extension);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruments_only_the_fuzz_kernel() {
        assert!(kernel_fuzz_args("kernel-x86_64", Kind::Debug).is_empty());
        assert!(kernel_fuzz_args("mikrosboot", Kind::Fuzz).is_empty());
        let args = kernel_fuzz_args("kernel-x86_64", Kind::Fuzz);
        assert_eq!(args[..3], ["--features", "fuzz", "--config"]);
        assert!(
            args[3].starts_with("target.x86_64-unknown-none.rustflags=[\"-Cpasses=sancov-module\"")
        );
    }

    #[test]
    fn builds_the_boot_archive() {
        let dir = crate::workspace_root().join("target/xtask-test");
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("init");
        fs::write(&source, b"\x7fELF test").unwrap();
        let path = boot_image(Kind::Fuzz, "boot.img", &[("init", &source)]).unwrap();
        assert!(path.ends_with("target/boot/fuzz/boot.img"));
        let bytes = fs::read(&path).unwrap();
        let mut archive = tar::Archive::new(bytes.as_slice());
        let mut entries = archive.entries().unwrap();
        let mut init = entries.next().unwrap().unwrap();
        assert_eq!(init.path().unwrap().to_str(), Some("init"));
        assert_eq!(init.header().entry_type(), tar::EntryType::Regular);
        assert_eq!(&init.header().as_bytes()[257..263], b"ustar\0");
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut init, &mut data).unwrap();
        assert_eq!(data, b"\x7fELF test");
        assert!(entries.next().is_none());
    }
}
