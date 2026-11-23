//! `cargo xtask build`: builds the system components.

use crate::components::{Component, Image, Role, Runtime};
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The build kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Production,
    Debug,
    Fuzz,
    /// The production build's optimization with checks and diagnostic
    /// records, for faults that show only in optimized builds.
    Diag,
}

impl Kind {
    /// The name of the kind on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Production => "production",
            Kind::Debug => "debug",
            Kind::Fuzz => "fuzz",
            Kind::Diag => "diag",
        }
    }

    /// The Cargo profile used for this kind.
    pub fn profile(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug => "dev",
            Kind::Fuzz => "fuzz",
            Kind::Diag => "diag",
        }
    }

    /// The directory Cargo writes the profile's output to.
    pub fn profile_dir(self) -> &'static str {
        match self {
            Kind::Production => "release",
            Kind::Debug => "debug",
            Kind::Fuzz => "fuzz",
            Kind::Diag => "diag",
        }
    }

    /// The flags that user-space programs of this kind are built with on
    /// top of their target's: the diagnostic build keeps frame pointers
    /// and sets the `mikros_diag` cfg, which turns on its records, and the
    /// fuzz build sets `mikros_fuzz`, by which tests allow more time.
    pub fn user_rustflags(self) -> &'static [&'static str] {
        match self {
            Kind::Diag => &["-Cforce-frame-pointers=yes", "--cfg=mikros_diag"],
            Kind::Fuzz => &["--cfg=mikros_fuzz"],
            _ => &[],
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// The build kind.
    #[arg(long, value_enum, default_value_t = Kind::Debug)]
    pub kind: Kind,
    /// The edition of the system: `server`, or `desktop`, which adds the
    /// compositor's stand-ins for the greeter and the shell.
    #[arg(long, value_enum, default_value_t = Edition::Server)]
    pub edition: Edition,
}

/// The paths of the built components.
pub struct Built {
    pub bootmenu: PathBuf,
    pub bootloader: PathBuf,
    pub kernel: PathBuf,
    /// An archive of the files that the system needs until the root file
    /// system is available, which the boot list of the system version
    /// names.
    pub boot_image: PathBuf,
    /// The whole system, early services included, which the root
    /// partition holds. For the test build it is the boot image.
    pub system_image: PathBuf,
    /// For the test build, the whole system of the setup run's root
    /// partition: the test boot image with the services that need the
    /// root file system.
    pub first_boot_image: Option<PathBuf>,
    /// For the test build, the root partition of the takeover run: the
    /// test boot image with a normal driver for QEMU's VGA function.
    pub takeover_image: Option<PathBuf>,
    /// For the build with the system tests, the system of the desktop
    /// run: the system image without the session settings.
    pub desktop_image: Option<PathBuf>,
    /// For the build with the system tests and a Dawn test program, the
    /// system of the Dawn run: the system without the system tests, with
    /// selftest, and with the program as its only test.
    pub dawn_image: Option<PathBuf>,
    /// With the Dawn run's system, the system of its desktop run: the
    /// desktop run's system with the Dawn window program as the shell
    /// probe's switcher part.
    pub dawn_desktop_image: Option<PathBuf>,
    /// Where xkbcommon was built.
    pub xkbcommon: crate::ports::Built,
}

/// The member of the session settings, which turn the desktop off.
const SESSION_SETTINGS: &str = "config/session.yaml";
/// The shell settings of the desktop run's system, by their member and
/// their file in the repository.
const PROBE_SHELL: (&str, &str) = (
    "config/desktop/shell.yaml",
    "user/tests/greeter-probe/shell.yaml",
);

/// The editions of the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Edition {
    /// The system whose users log in on the text console.
    Server,
    /// The system with the compositor and the stand-ins for the greeter and
    /// the shell, whose user is logged in by the greeter. For development.
    Desktop,
}

impl Edition {
    /// The boot image that builds this edition.
    fn boot_image(self) -> BootImage {
        match self {
            Edition::Server => BootImage::System,
            Edition::Desktop => BootImage::Desktop,
        }
    }
}

/// The boot images that can be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootImage {
    /// The system's boot image.
    System,
    /// The system's boot image, with the system of the desktop edition: the
    /// compositor's stand-ins for the greeter and the shell, and the user
    /// that the greeter logs in.
    Desktop,
    /// The system's boot image, with a test image's system: the system
    /// tests and selftest besides the system's components.
    SystemTests,
    /// The boot image of the integration tests, whose init runs them.
    Test,
}

/// Builds every component of the build kind `kind`.
pub fn build(kind: Kind) -> Result<Built> {
    build_edition(kind, Edition::Server)
}

/// Builds every component of the build kind `kind`, with the system of the
/// edition `edition`.
pub fn build_edition(kind: Kind, edition: Edition) -> Result<Built> {
    build_for(kind, edition.boot_image())
}

/// Builds every component of the build kind `kind`, and a test image's
/// system as its system image.
pub fn build_tests(kind: Kind) -> Result<Built> {
    build_for(kind, BootImage::SystemTests)
}

/// The programs of the Dawn run.
pub struct DawnTest<'a> {
    /// The program that is a system test of the Dawn run's system.
    pub compute: &'a Path,
    /// The program that is the other system test of the Dawn run's system.
    pub shared: &'a Path,
    /// The program that is the shell probe's switcher part in the Dawn
    /// run's desktop system.
    pub window: &'a Path,
}

/// Builds every component of the build kind `kind`, and a test image's
/// system as its system image, and the Dawn run's two systems with the
/// programs of `dawn`.
pub fn build_dawn_test(kind: Kind, dawn: &DawnTest) -> Result<Built> {
    build_for_with(kind, BootImage::SystemTests, Some(dawn))
}

/// Builds every component of the build kind `kind`, with `boot` as the
/// boot image.
pub fn build_for(kind: Kind, boot: BootImage) -> Result<Built> {
    build_for_with(kind, boot, None)
}

/// Builds every component of the build kind `kind`, with `boot` as the
/// boot image, and with the test image with system tests also the Dawn
/// run's system images when there are Dawn programs `dawn_test`.
fn build_for_with(kind: Kind, boot: BootImage, dawn_test: Option<&DawnTest>) -> Result<Built> {
    // The compositor's build script reads them.
    crate::wayland::protocols()?;
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
    if matches!(kind, Kind::Debug | Kind::Diag) {
        crate::debug_build::check(&kernel)?;
        println!(
            "checked  {} has the {} build's settings",
            kernel.display(),
            kind.name()
        );
    }
    let debug_info = crate::debuginfo::split(&kernel)?;
    println!("built    {}", debug_info.display());
    if kind == Kind::Fuzz {
        let dir = syzkaller_descriptions()?;
        println!("built    {}", dir.display());
    }
    let xkbcommon_port = crate::ports::load("xkbcommon")?;
    let xkbcommon = crate::ports::build(&xkbcommon_port)?;
    crate::build_systems::check_installed(
        &xkbcommon.install,
        &[
            "libraries/libxkbcommon.dlib",
            "include/xkbcommon/xkbcommon.h",
            "bin/xkbcli",
        ],
        &[],
    )?;
    crate::build_systems::check_sonames(&xkbcommon.install, &["libxkbcommon.dlib"])?;
    println!("built    {}", xkbcommon.install.display());
    let keymaps = crate::ports::build(&crate::ports::load("xkeyboard-config")?)?;
    crate::build_systems::check_installed(
        &keymaps.install,
        &[
            "share/xkeyboard-config-2/rules/evdev",
            "share/xkeyboard-config-2/symbols/us",
            "share/xkeyboard-config-2/keycodes/evdev",
        ],
        &[],
    )?;
    println!("built    {}", keymaps.install.display());
    let freetype = crate::ports::build(&crate::ports::load("freetype")?)?;
    crate::build_systems::check_installed(
        &freetype.install,
        &[
            "libraries/libfreetype.dlib",
            "include/freetype2/ft2build.h",
            "include/freetype2/freetype/freetype.h",
        ],
        &[],
    )?;
    crate::build_systems::check_sonames(&freetype.install, &["libfreetype.dlib"])?;
    println!("built    {}", freetype.install.display());
    let wayland = crate::wayland::build()?;
    println!("built    {}", wayland.library.display());
    let llvm = crate::llvm_port::build_library()?;
    println!("built    {}", llvm.display());
    let mesa = crate::mesa::build()?;
    println!("built    {}", mesa.display());
    let vulkan = crate::vulkan::loader()?;
    println!("built    {}", vulkan.display());
    let sdl = crate::sdl::build()?;
    println!("built    {}", sdl.display());
    let fonts = crate::fonts::fetch()?;
    println!("ok       {}", fonts.ttf.display());
    let mut first_boot_image = None;
    let mut takeover_image = None;
    let mut desktop_image = None;
    let mut dawn_image = None;
    let mut dawn_desktop_image = None;
    let (boot_image, system_image) = match boot {
        BootImage::System | BootImage::Desktop | BootImage::SystemTests => {
            let tests = boot == BootImage::SystemTests;
            // The desktop edition and the test images have the stand-ins
            // for the greeter and the shell.
            let with_desktop = tests || boot == BootImage::Desktop;
            let headers = crate::vulkan::headers()?.join("include");
            let components = crate::components::all()?;
            // What only test images hold is built for them alone.
            let built = |runtime| -> Vec<String> {
                components
                    .iter()
                    .filter(|c| c.runtime == runtime && c.in_kind(kind))
                    .filter(|c| tests || c.image != Image::Tests)
                    .map(|c| c.name.clone())
                    .collect()
            };
            prebuild(&built(Runtime::Rt), kind)?;
            crate::rust_std::prebuild(&built(Runtime::Std), kind)?;
            // The components of the boot image: init, the managers, the
            // services that start before the root file system is
            // available, and the drivers, which the device manager reads
            // from the boot image only until then.
            let mut early = Vec::new();
            let mut only_tests = Vec::new();
            // What only the test images hold, besides the system tests,
            // selftest for one: the Dawn run's system has these.
            let mut test_support = Vec::new();
            for component in components.iter().filter(|c| c.in_kind(kind)) {
                let members = match component.image {
                    Image::Boot => &mut early,
                    Image::Tests if tests => &mut only_tests,
                    _ => continue,
                };
                let program = component_build(component, kind)?;
                let own = component.members(kind, &program)?;
                if component.image == Image::Tests && component.role != Role::Test {
                    test_support.extend(own.iter().cloned());
                }
                members.extend(own);
            }
            if tests {
                // Two system tests in C, which selftest runs as it runs
                // the others: the C bindings of the MIDL libraries, built
                // without warnings.
                let strict = ["-Wall".to_string(), "-Wextra".into(), "-Werror".into()];
                for (name, source, linking) in [
                    ("midl-c", "user/tests/midl-c/midl.c", Linking::Static),
                    (
                        "midl-cxx",
                        "user/tests/midl-c/headers.cpp",
                        Linking::Dynamic,
                    ),
                ] {
                    let program = c_build(name, &[source], linking, &strict)?;
                    println!("built    {}", program.display());
                    only_tests.push((format!("system/bin/tests/{name}"), program));
                }
                // Vulkan through the loader and the CPU driver, with a
                // compute shader that glslang compiles here into a header.
                let shaders = c_dir("vulkan-c");
                fs::create_dir_all(&shaders)?;
                crate::port::run(
                    Command::new(crate::glslang::build()?.join("glslangValidator"))
                        .args(["-V", "--quiet", "--vn", "square_spv", "-o"])
                        .arg(shaders.join("square.h"))
                        .arg(crate::workspace_root().join("user/tests/vulkan-c/square.comp")),
                )
                .context("compiling the shader of vulkan-c")?;
                let mut args = strict.to_vec();
                args.extend([
                    format!("-I{}", headers.display()),
                    format!("-I{}", shaders.display()),
                    vulkan
                        .join("libraries")
                        .join(crate::vulkan::LOADER)
                        .display()
                        .to_string(),
                ]);
                let program = c_build(
                    "vulkan-c",
                    &["user/tests/vulkan-c/vulkan.c"],
                    Linking::Dynamic,
                    &args,
                )?;
                println!("built    {}", program.display());
                only_tests.push(("system/bin/tests/vulkan-c".to_owned(), program));
            }
            // The stand-ins for the greeter and the shell, which are test
            // programs and system apps.
            let desktop_apps = if with_desktop {
                greeter_probe(&wayland, &vulkan, &headers, &sdl, &xkbcommon)?
            } else {
                Vec::new()
            };
            // The system fonts and the fontconfig configuration that names
            // them as the defaults.
            for file in crate::fonts::FILES {
                early.push((
                    format!("system/share/fonts/dejavu/{file}"),
                    fonts.ttf.join(file),
                ));
            }
            early.extend([
                (
                    "system/share/fonts/dejavu/LICENSE".to_owned(),
                    fonts.license.clone(),
                ),
                (
                    "system/share/fonts/fonts.conf".to_owned(),
                    crate::workspace_root().join("user/data/fonts.conf"),
                ),
            ]);
            // The time zones, which the root partition gets with the rest
            // of the system version.
            early.extend(crate::tzdata::members(&crate::tzdata::zoneinfo()?)?);
            // The services that need the root file system, and what they
            // load, are only in the whole system, so that the service
            // manager starts them once the root is available.
            let login = login_members(
                kind,
                &xkbcommon,
                &freetype,
                &keymaps,
                &wayland.library,
                &vulkan_members(&vulkan, &mesa, &llvm),
            )?;
            let whole: Vec<(&str, &Path)> = early
                .iter()
                .chain(&login)
                .chain(&only_tests)
                .chain(&desktop_apps)
                .map(|(at, path)| (at.as_str(), path.as_path()))
                .collect();
            // The desktop system: the whole system without the session
            // settings, so that a desktop counts as installed, with shell
            // settings that name the greeter probe, and with the user that
            // the probe logs in.
            let shell = crate::workspace_root().join(PROBE_SHELL.1);
            let users = if with_desktop {
                desktop_users(kind)?
            } else {
                PathBuf::new()
            };
            let desktop_members: Vec<(&str, &Path)> = if with_desktop {
                whole
                    .iter()
                    .copied()
                    .filter(|&(at, _)| at != SESSION_SETTINGS)
                    .chain([
                        (PROBE_SHELL.0, shell.as_path()),
                        ("state/accounts/users", users.as_path()),
                    ])
                    .collect()
            } else {
                Vec::new()
            };
            if !tests {
                check_no_tests(&whole)?;
                check_no_tests(&desktop_members)?;
            }
            let (name, members) = match boot {
                BootImage::SystemTests => ("system-tests.img", &whole),
                BootImage::Desktop => ("system-desktop.img", &desktop_members),
                _ => ("system.img", &whole),
            };
            let system_image = boot_image(kind, name, members)?;
            println!("built    {}", system_image.display());
            if tests {
                let desktop = boot_image(kind, "desktop.img", &desktop_members)?;
                println!("built    {}", desktop.display());
                desktop_image = Some(desktop);
                if let Some(dawn_test) = dawn_test {
                    // The Dawn run's system: the system with selftest
                    // and what it needs of the test images, and the
                    // two Dawn test programs as the only tests.
                    let at = "system/bin/tests/webgpu";
                    let dawn: Vec<(&str, &Path)> = early
                        .iter()
                        .chain(&login)
                        .chain(&test_support)
                        .map(|(at, path)| (at.as_str(), path.as_path()))
                        .chain([
                            (at, dawn_test.compute),
                            ("system/bin/tests/dawn-shared", dawn_test.shared),
                        ])
                        .collect();
                    let loader = format!("system/libraries/{}", crate::vulkan::LOADER);
                    for needed in [
                        "system/bin/selftest",
                        "system/libraries/libc++.dlib",
                        loader.as_str(),
                    ] {
                        if !dawn.iter().any(|(at, _)| *at == needed) {
                            bail!("the Dawn run's system has no {needed}");
                        }
                    }
                    let image = boot_image(kind, "dawn-tests.img", &dawn)?;
                    println!("built    {}", image.display());
                    dawn_image = Some(image);
                    // The Dawn run's desktop system: the desktop run's
                    // system with its shell probe's switcher part replaced
                    // by the Dawn window program.
                    let folder = format!("system/apps/{SHELL_PROBE}/");
                    let greeter = desktop_members
                        .iter()
                        .find(|(at, _)| *at == format!("{folder}greeter-probe"))
                        .map(|(_, path)| path.to_path_buf())
                        .context("the desktop system has no shell probe")?;
                    let parts = shell_probe_parts(DAWN_SWITCHER);
                    let window = dawn_test.window.to_path_buf();
                    let shell = shell_probe(&parts, greeter, DAWN_SWITCHER, window);
                    let dir = c_dir(DAWN_SWITCHER).join("app");
                    let members = crate::system_apps::members(&shell, &dir)?;
                    let dawn_desktop: Vec<(&str, &Path)> = desktop_members
                        .iter()
                        .copied()
                        .filter(|(at, _)| !at.starts_with(&folder))
                        .chain(
                            members
                                .iter()
                                .map(|(at, path)| (at.as_str(), path.as_path())),
                        )
                        .collect();
                    let image = boot_image(kind, "dawn-desktop.img", &dawn_desktop)?;
                    println!("built    {}", image.display());
                    dawn_desktop_image = Some(image);
                }
            }
            let early: Vec<(&str, &Path)> = early
                .iter()
                .map(|(at, path)| (at.as_str(), path.as_path()))
                .collect();
            (boot_image(kind, "boot.img", &early)?, system_image)
        }
        BootImage::Test => {
            // Every program of the workspaces, but the test init of a fuzz
            // build, which is built with a feature of its own.
            let programs = crate::components::programs()?;
            let built = |runtime| -> Vec<String> {
                programs
                    .iter()
                    .filter(|(p, r)| *r == runtime && !(kind == Kind::Fuzz && p == "test-init"))
                    .map(|(p, _)| p.clone())
                    .collect()
            };
            prebuild(&built(Runtime::Rt), kind)?;
            crate::rust_std::prebuild(&built(Runtime::Std), kind)?;
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
            let manager = user_build("processmgr", kind)?;
            println!("built    {}", manager.display());
            let services = user_build("servicemgr", kind)?;
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
            let memfd_c = c_build(
                "memfd-c",
                &["user/tests/memfd-c/memfd.c"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", memfd_c.display());
            let handles_c = c_build(
                "handles-c",
                &["user/tests/handles-c/handles.c"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", handles_c.display());
            let wayland_c = c_build(
                "wayland-c",
                &["user/tests/wayland-c/wayland.c"],
                Linking::Dynamic,
                &[
                    format!("-I{}", wayland.install.join("include").display()),
                    wayland.library.display().to_string(),
                ],
            )?;
            println!("built    {}", wayland_c.display());
            let cxx_dynamic = c_build("cxx", &["user/tests/cxx/cxx.cpp"], Linking::Dynamic, &[])?;
            println!("built    {}", cxx_dynamic.display());
            let cxx_static = c_build(
                "cxx-static",
                &["user/tests/cxx/cxx.cpp"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", cxx_static.display());
            let sysroot_lib = crate::sysroot::dir().join("libraries");
            let global_check = crate::rust_std::build("global-check", kind)?;
            let forge_probe = crate::rust_std::build("forge-probe", kind)?;
            let forge_manifest =
                crate::workspace_root().join("user/tests/forge-probe/system.forge-probe.yaml");
            let files_c = c_build(
                "files-c",
                &["user/tests/files-c/files.c"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", files_c.display());
            // Linked with the C library also by file name, which the
            // driver finds in the sysroot's libraries without a -L
            // (MIKSPEC-0062.R16).
            let hello_dynamic = c_build(
                "hello-c-dynamic",
                &["user/tests/hello-c/hello.c"],
                Linking::Dynamic,
                &["-l:libc.dlib".to_string()],
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
            let spawn_c = crate::workspace_root().join("user/tests/spawn-c");
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
                    "-Wl,--disable-new-dtags,-rpath,/system/share/decoy".to_owned(),
                ],
            )?;
            println!("built    {}", library_user.display());
            // The same project built with CMake and with Meson, through
            // the sysroot's toolchain files.
            let tally_source = crate::workspace_root().join("user/tests/build-systems");
            let cmake_tree = crate::build_systems::cmake("tally-cmake", &tally_source, &[])?;
            let meson_tree = crate::build_systems::meson("tally-meson", &tally_source, &[])?;
            for (tree, system) in [(&cmake_tree, "cmake"), (&meson_tree, "meson")] {
                // Both libraries are named without their versions, the
                // one the project gives and the one it does not.
                let real = format!("libtally-{system}.dlib");
                let base = format!("libbase-{system}.dlib");
                crate::build_systems::check_installed(
                    tree,
                    &[
                        &format!("bin/tally-{system}-user"),
                        &format!("libraries/{real}"),
                        &format!("libraries/{base}"),
                        &format!("libraries/libcount-{system}.slib"),
                    ],
                    &[],
                )?;
                crate::build_systems::check_sonames(tree, &[&real, &base])?;
                println!("built    {}", tree.display());
            }
            let hello_std = crate::rust_std::build("hello-std", kind)?;
            println!("built    {}", hello_std.display());
            let spawn_std = crate::rust_std::build("spawn-std", kind)?;
            println!("built    {}", spawn_std.display());
            let msh = crate::rust_std::build("msh", kind)?;
            println!("built    {}", msh.display());
            let msh_check = crate::rust_std::build("msh-check", kind)?;
            println!("built    {}", msh_check.display());
            let msh_options = crate::workspace_root().join("user/tests/msh-check/options.msh");
            let programs = crate::workspace_root().join("user/tests/launcher-probe/programs");
            let (prog, closed, bad, command, sessions) = (
                programs.join("system.prog.yaml"),
                programs.join("system.closed.yaml"),
                programs.join("system.bad.yaml"),
                programs.join("system.probe-command.yaml"),
                programs.join("system.sessionmgr.yaml"),
            );
            let dynamic_linker = crate::sysroot::dir().join("libraries/ld-mikros.dlib");
            let greeting = crate::workspace_root().join("user/tests/hello-c/greeting.txt");
            // The process manager tests serve this image, in which the
            // probe takes the service manager's place.
            let launcher = archive(
                kind,
                "launcher.img",
                &[
                    ("system/servicemgr", probe.as_path()),
                    ("system/bin/probe", probe.as_path()),
                    ("system/bin/hello", hello.as_path()),
                    ("system/bin/hello-dynamic", hello_dynamic.as_path()),
                    ("system/bin/raw-syscall", raw_syscall.as_path()),
                    ("system/bin/memfd-c", memfd_c.as_path()),
                    ("system/bin/handles-c", handles_c.as_path()),
                    // Applications that the boot image provides.
                    ("apps/test/bin/hello", hello.as_path()),
                    ("apps/test/bin/hello-dynamic", hello_dynamic.as_path()),
                    ("apps/test/bin/raw-syscall", raw_syscall.as_path()),
                    // An app's bundle, and program manifests.
                    ("apps/test.app/bin/hello", hello_dynamic.as_path()),
                    ("system/programs/system.prog.yaml", prog.as_path()),
                    ("system/programs/system.closed.yaml", closed.as_path()),
                    ("system/programs/system.bad.yaml", bad.as_path()),
                    // The session manager's app ID, which the probe takes
                    // to drive Elevations.
                    ("system/programs/system.sessionmgr.yaml", sessions.as_path()),
                    // A command, which the probe runs as another binary.
                    ("system/bin/probe-command", probe.as_path()),
                    (
                        "system/programs/system.probe-command.yaml",
                        command.as_path(),
                    ),
                    ("system/bin/hello-std", hello_std.as_path()),
                    ("system/bin/spawn-std", spawn_std.as_path()),
                    ("system/bin/msh", msh.as_path()),
                    ("system/bin/msh-check", msh_check.as_path()),
                    ("system/share/msh/options.msh", msh_options.as_path()),
                    ("system/bin/spawn-child", spawn_child.as_path()),
                    // Scripts and a text file, which hello-c starts.
                    ("system/bin/spawn-script", &spawn_c.join("spawn-script")),
                    (
                        "system/bin/script-of-script",
                        &spawn_c.join("script-of-script"),
                    ),
                    (
                        "system/bin/script-without-interpreter",
                        &spawn_c.join("script-without-interpreter"),
                    ),
                    ("system/bin/not-a-program", &spawn_c.join("not-a-program")),
                    ("system/libraries/ld-mikros.dlib", dynamic_linker.as_path()),
                    (
                        "system/libraries/libwayland.dlib",
                        wayland.library.as_path(),
                    ),
                    ("system/bin/wayland-c", wayland_c.as_path()),
                    ("system/bin/cxx", cxx_dynamic.as_path()),
                    ("system/bin/cxx-static", cxx_static.as_path()),
                    (
                        "system/libraries/libc++.dlib",
                        &sysroot_lib.join("libc++.dlib"),
                    ),
                    (
                        "system/libraries/libc++abi.dlib",
                        &sysroot_lib.join("libc++abi.dlib"),
                    ),
                    (
                        "system/libraries/libunwind.dlib",
                        &sysroot_lib.join("libunwind.dlib"),
                    ),
                    ("system/bin/library-user", library_user.as_path()),
                    ("system/libraries/libgreet.1.dlib", greet.as_path()),
                    ("system/share/decoy/libgreet.1.dlib", decoy.as_path()),
                    ("system/libraries/libnonote.1.dlib", no_note.as_path()),
                    (
                        "system/bin/tally-cmake-user",
                        &cmake_tree.join("bin/tally-cmake-user"),
                    ),
                    (
                        "system/libraries/libtally-cmake.dlib",
                        &cmake_tree.join("libraries/libtally-cmake.dlib"),
                    ),
                    (
                        "system/libraries/libbase-cmake.dlib",
                        &cmake_tree.join("libraries/libbase-cmake.dlib"),
                    ),
                    (
                        "system/bin/tally-meson-user",
                        &meson_tree.join("bin/tally-meson-user"),
                    ),
                    (
                        "system/libraries/libtally-meson.dlib",
                        &meson_tree.join("libraries/libtally-meson.dlib"),
                    ),
                    (
                        "system/libraries/libbase-meson.dlib",
                        &meson_tree.join("libraries/libbase-meson.dlib"),
                    ),
                    ("system/share/hello/greeting.txt", greeting.as_path()),
                ],
                &[],
                &[
                    ("system/share/hello/near", "greeting.txt"),
                    ("system/share/hello/far", "/system/share/hello/greeting.txt"),
                    ("system/bin/app-hello", "../../apps/test/bin/hello"),
                ],
            )?;
            println!("built    {}", launcher.display());
            let runner = user_build("port-tests", kind)?;
            println!("built    {}", runner.display());
            // The ported software test serves this image, in which the
            // runner of the ported projects' tests takes the service
            // manager's place.
            let plan = crate::workspace_root().join("target/ported/port-tests.plan");
            let mut members = crate::port_tests::members(&xkbcommon_port, &xkbcommon, &plan)?;
            members.push(("system/servicemgr".into(), runner));
            members.push((
                "system/libraries/ld-mikros.dlib".into(),
                dynamic_linker.clone(),
            ));
            let members: Vec<(&str, &Path)> = members
                .iter()
                .map(|(n, p)| (n.as_str(), p.as_path()))
                .collect();
            let ports_image = test_image(kind, "ports.img", &members)?;
            println!("built    {}", ports_image.display());
            let broker = user_build("permbroker", kind)?;
            println!("built    {}", broker.display());
            let broker_probe = user_build("broker-probe", kind)?;
            println!("built    {}", broker_probe.display());
            // The broker tests serve this image, in which the broker
            // probe takes the service manager's place.
            let broker_image = test_image(
                kind,
                "broker.img",
                &[
                    ("system/servicemgr", broker_probe.as_path()),
                    (
                        "system/services/system.permbroker/permbroker",
                        broker.as_path(),
                    ),
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
                crate::workspace_root().join("user/services/permbroker/permbroker.yaml");
            let services_image = test_image(
                kind,
                "services.img",
                &[
                    ("system/servicemgr", services.as_path()),
                    (
                        "system/services/system.permbroker/permbroker",
                        broker.as_path(),
                    ),
                    (
                        "system/services/system.permbroker/service.yaml",
                        broker_manifest.as_path(),
                    ),
                    (
                        "system/services/system.directory-probe/directory-probe",
                        directory_probe.as_path(),
                    ),
                    (
                        "system/services/system.directory-probe/service.yaml",
                        directory_probe_manifest.as_path(),
                    ),
                    (
                        "system/services/system.restart-probe/service.yaml",
                        restart_probe_manifest.as_path(),
                    ),
                    (
                        "system/services/system.demand-probe/service.yaml",
                        demand_probe_manifest.as_path(),
                    ),
                    (
                        "system/services/system.demand-dep/service.yaml",
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
            let tmpfs_manifest = crate::workspace_root().join("user/services/tmpfs/tmpfs.yaml");
            let tmpfs_probe_manifest =
                crate::workspace_root().join("user/tests/tmpfs-probe/tmpfs-probe.yaml");
            let union = user_build("union", kind)?;
            println!("built    {}", union.display());
            let union_probe = user_build("union-probe", kind)?;
            println!("built    {}", union_probe.display());
            let union_manifest = crate::workspace_root().join("user/services/union/union.yaml");
            let union_probe_manifest =
                crate::workspace_root().join("user/tests/union-probe/union-probe.yaml");
            let tmpfs_image = test_image(
                kind,
                "tmpfs.img",
                &[
                    ("system/servicemgr", services.as_path()),
                    (
                        "system/services/system.permbroker/permbroker",
                        broker.as_path(),
                    ),
                    (
                        "system/services/system.permbroker/service.yaml",
                        broker_manifest.as_path(),
                    ),
                    ("system/services/system.tmpfs/tmpfs", tmpfs.as_path()),
                    (
                        "system/services/system.tmpfs/service.yaml",
                        tmpfs_manifest.as_path(),
                    ),
                    (
                        "system/services/system.tmpfs-probe/tmpfs-probe",
                        tmpfs_probe.as_path(),
                    ),
                    (
                        "system/services/system.tmpfs-probe/service.yaml",
                        tmpfs_probe_manifest.as_path(),
                    ),
                    ("system/services/system.union/union", union.as_path()),
                    (
                        "system/services/system.union/service.yaml",
                        union_manifest.as_path(),
                    ),
                    (
                        "system/services/system.union-probe/union-probe",
                        union_probe.as_path(),
                    ),
                    (
                        "system/services/system.union-probe/service.yaml",
                        union_probe_manifest.as_path(),
                    ),
                ],
            )?;
            println!("built    {}", tmpfs_image.display());
            // The log service test serves this image: the service manager,
            // the broker, the log service and a probe that writes and reads
            // the log, with their manifests.
            let log_service = user_build("logsvc", kind)?;
            println!("built    {}", log_service.display());
            let log_manifest = crate::workspace_root().join("user/services/logsvc/logsvc.yaml");
            let log_probe = user_build("log-probe", kind)?;
            println!("built    {}", log_probe.display());
            let log_probe_manifest =
                crate::workspace_root().join("user/tests/log-probe/log-probe.yaml");
            let log_image = test_image(
                kind,
                "log.img",
                &[
                    ("system/servicemgr", services.as_path()),
                    (
                        "system/services/system.permbroker/permbroker",
                        broker.as_path(),
                    ),
                    (
                        "system/services/system.permbroker/service.yaml",
                        broker_manifest.as_path(),
                    ),
                    (
                        "system/services/system.logsvc/logsvc",
                        log_service.as_path(),
                    ),
                    (
                        "system/services/system.logsvc/service.yaml",
                        log_manifest.as_path(),
                    ),
                    (
                        "system/services/system.log-probe/log-probe",
                        log_probe.as_path(),
                    ),
                    (
                        "system/services/system.log-probe/service.yaml",
                        log_probe_manifest.as_path(),
                    ),
                ],
            )?;
            println!("built    {}", log_image.display());
            // The time service test serves this image: the service manager,
            // the broker, the time service and the time probe, with their
            // manifests.
            let time_service = user_build("timesvc", kind)?;
            println!("built    {}", time_service.display());
            let time_manifest = crate::workspace_root().join("user/services/timesvc/timesvc.yaml");
            let time_probe = user_build("time-probe", kind)?;
            println!("built    {}", time_probe.display());
            let probe_dir = crate::workspace_root().join("user/tests/time-probe");
            let time_image = test_image(
                kind,
                "time.img",
                &[
                    ("system/servicemgr", services.as_path()),
                    (
                        "system/services/system.permbroker/permbroker",
                        broker.as_path(),
                    ),
                    (
                        "system/services/system.permbroker/service.yaml",
                        broker_manifest.as_path(),
                    ),
                    (
                        "system/services/system.timesvc/timesvc",
                        time_service.as_path(),
                    ),
                    (
                        "system/services/system.timesvc/service.yaml",
                        time_manifest.as_path(),
                    ),
                    (
                        "system/services/system.time-probe/time-probe",
                        time_probe.as_path(),
                    ),
                    (
                        "system/services/system.time-probe/service.yaml",
                        probe_dir.join("time-probe.yaml").as_path(),
                    ),
                ],
            )?;
            println!("built    {}", time_image.display());
            // The terminal service test serves this image: the service
            // manager, the broker, the terminal service and its probe,
            // with their manifests, and the C program that the probe
            // runs on a terminal.
            let term_service = user_build("termsvc", kind)?;
            println!("built    {}", term_service.display());
            let term_probe = user_build("term-probe", kind)?;
            println!("built    {}", term_probe.display());
            let term_c = c_build(
                "term-c",
                &["user/tests/term-c/term.c"],
                Linking::Static,
                &[],
            )?;
            println!("built    {}", term_c.display());
            let term_image = test_image(
                kind,
                "term.img",
                &[
                    ("system/servicemgr", services.as_path()),
                    (
                        "system/services/system.permbroker/permbroker",
                        broker.as_path(),
                    ),
                    (
                        "system/services/system.permbroker/service.yaml",
                        broker_manifest.as_path(),
                    ),
                    (
                        "system/services/system.termsvc/termsvc",
                        term_service.as_path(),
                    ),
                    (
                        "system/services/system.termsvc/service.yaml",
                        crate::workspace_root()
                            .join("user/services/termsvc/termsvc.yaml")
                            .as_path(),
                    ),
                    (
                        "system/services/system.term-probe/term-probe",
                        term_probe.as_path(),
                    ),
                    ("system/bin/term-c", term_c.as_path()),
                    (
                        "system/services/system.term-probe/service.yaml",
                        crate::workspace_root()
                            .join("user/tests/term-probe/term-probe.yaml")
                            .as_path(),
                    ),
                ],
            )?;
            println!("built    {}", term_image.display());
            // The namespaces run's root partition holds these files: the
            // temporary file system and a probe of its namespace,
            // with their manifests, files in /system/config, /system and /libraries
            // that the probe reads, and a native app's bundle with the
            // dynamic linker it needs.
            let namespace_probe = user_build("namespace-probe", kind)?;
            println!("built    {}", namespace_probe.display());
            let probe_dir = crate::workspace_root().join("user/tests/namespace-probe");
            let namespace_probe_manifest = probe_dir.join("namespace-probe.yaml");
            let probe_conf = probe_dir.join("probe.conf");
            let probe_data = probe_dir.join("data.txt");
            let programs = probe_dir.join("programs");
            let namespaces_files: Vec<(String, PathBuf)> = vec![
                (
                    "system/services/system.tmpfs/tmpfs".to_owned(),
                    tmpfs.to_path_buf(),
                ),
                (
                    "system/services/system.tmpfs/service.yaml".to_owned(),
                    tmpfs_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.namespace-probe/namespace-probe".to_owned(),
                    namespace_probe.to_path_buf(),
                ),
                (
                    "system/services/system.namespace-probe/service.yaml".to_owned(),
                    namespace_probe_manifest.to_path_buf(),
                ),
                (
                    "system/share/probe/data.txt".to_owned(),
                    probe_data.to_path_buf(),
                ),
                ("config/probe.conf".to_owned(), probe_conf.to_path_buf()),
                ("libraries/probe.txt".to_owned(), probe_data.to_path_buf()),
                (
                    "apps/test.native/bin/hello".to_owned(),
                    hello_dynamic.to_path_buf(),
                ),
                (
                    "system/libraries/ld-mikros.dlib".to_owned(),
                    dynamic_linker.to_path_buf(),
                ),
                (
                    "system/programs/system.probe-tool.yaml".to_owned(),
                    programs.join("system.probe-tool.yaml").to_path_buf(),
                ),
                (
                    "system/programs/system.tmpfs.yaml".to_owned(),
                    programs.join("system.tmpfs.yaml").to_path_buf(),
                ),
                (
                    "system/programs/system.idle.yaml".to_owned(),
                    programs.join("system.idle.yaml").to_path_buf(),
                ),
                (
                    "system/services/system.idle/service.yaml".to_owned(),
                    probe_dir.join("idle.yaml").to_path_buf(),
                ),
                (
                    "system/programs/system.misnamed.yaml".to_owned(),
                    programs.join("system.misnamed.yaml").to_path_buf(),
                ),
            ];
            // The accounts run's root partition holds these files: the
            // temporary file system and the accounts service, with their
            // manifests, the probe's binary with the manifests of its
            // five services, and a users file in /system/state/accounts.
            let accounts = user_build("accountsvc", kind)?;
            println!("built    {}", accounts.display());
            let accounts_probe = user_build("accounts-probe", kind)?;
            println!("built    {}", accounts_probe.display());
            let accounts_manifest =
                crate::workspace_root().join("user/services/accountsvc/accountsvc.yaml");
            let probe_dir = crate::workspace_root().join("user/tests/accounts-probe");
            let accounts_probe_manifest = probe_dir.join("accounts-probe.yaml");
            let accounts_guest_manifest = probe_dir.join("accounts-guest.yaml");
            let stub_manifest = probe_dir.join("authorization-stub.yaml");
            let thief_manifest = probe_dir.join("accounts-thief.yaml");
            let sneak_manifest = probe_dir.join("accounts-sneak.yaml");
            let users = accounts_users(kind)?;
            let accounts_files: Vec<(String, PathBuf)> = vec![
                (
                    "system/services/system.tmpfs/tmpfs".to_owned(),
                    tmpfs.to_path_buf(),
                ),
                (
                    "system/services/system.tmpfs/service.yaml".to_owned(),
                    tmpfs_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.accountsvc/accountsvc".to_owned(),
                    accounts.to_path_buf(),
                ),
                (
                    "system/services/system.accountsvc/service.yaml".to_owned(),
                    accounts_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.accounts-probe/accounts-probe".to_owned(),
                    accounts_probe.to_path_buf(),
                ),
                (
                    "system/services/system.accounts-probe/service.yaml".to_owned(),
                    accounts_probe_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.accounts-guest/service.yaml".to_owned(),
                    accounts_guest_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.authorization-stub/service.yaml".to_owned(),
                    stub_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.accounts-thief/service.yaml".to_owned(),
                    thief_manifest.to_path_buf(),
                ),
                (
                    "system/services/system.accounts-sneak/service.yaml".to_owned(),
                    sneak_manifest.to_path_buf(),
                ),
                ("state/accounts/users".to_owned(), users.to_path_buf()),
            ];
            // The device manager tests serve these images. Each holds the
            // service manager, the broker and the device manager with
            // their manifests, and test drivers: their manifests from
            // user/tests/drivers/, and the test driver's binary for those
            // that start.
            let devices = user_build("devicemgr", kind)?;
            println!("built    {}", devices.display());
            let test_driver = user_build("test-driver", kind)?;
            println!("built    {}", test_driver.display());
            let pci_bus = user_build("pcibus", kind)?;
            println!("built    {}", pci_bus.display());
            let virtio_blk = user_build("virtioblk", kind)?;
            println!("built    {}", virtio_blk.display());
            let nvme = user_build("nvme", kind)?;
            println!("built    {}", nvme.display());
            let ahci = user_build("ahci", kind)?;
            println!("built    {}", ahci.display());
            let xhci = user_build("xhci", kind)?;
            println!("built    {}", xhci.display());
            let usb_storage = user_build("usbstorage", kind)?;
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
            let ps2 = user_build("ps2", kind)?;
            println!("built    {}", ps2.display());
            let usb_hid = user_build("usbhid", kind)?;
            println!("built    {}", usb_hid.display());
            let power_probe = user_build("power-probe", kind)?;
            println!("built    {}", power_probe.display());
            let devices_manifest =
                crate::workspace_root().join("user/services/devicemgr/devicemgr.yaml");
            let device_image = |file: &str,
                                drivers: &[(&str, bool)],
                                more: &[(String, PathBuf)]|
             -> Result<PathBuf> {
                let mut members: Vec<(String, PathBuf)> = vec![
                    ("system/servicemgr".into(), services.clone()),
                    (
                        "system/services/system.permbroker/permbroker".into(),
                        broker.clone(),
                    ),
                    (
                        "system/services/system.permbroker/service.yaml".into(),
                        broker_manifest.clone(),
                    ),
                    (
                        "system/services/system.devicemgr/devicemgr".into(),
                        devices.clone(),
                    ),
                    (
                        "system/services/system.devicemgr/service.yaml".into(),
                        devices_manifest.clone(),
                    ),
                ];
                for &(name, starts) in drivers {
                    let manifest = format!("user/tests/drivers/{name}/driver.yaml");
                    members.push((
                        format!("system/drivers/system.driver.{name}/driver.yaml"),
                        crate::workspace_root().join(manifest),
                    ));
                    if starts {
                        let binary = format!("system/drivers/system.driver.{name}/{name}");
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
                    (
                        "system/drivers/system.driver.acpi/acpi".to_owned(),
                        acpi.clone(),
                    ),
                    (
                        "system/drivers/system.driver.acpi/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/acpi/driver.yaml"),
                    ),
                    // The log service, through which the drivers' messages
                    // reach the kernel log.
                    (
                        "system/services/system.logsvc/logsvc".to_owned(),
                        log_service.clone(),
                    ),
                    (
                        "system/services/system.logsvc/service.yaml".to_owned(),
                        log_manifest.clone(),
                    ),
                    (
                        "system/drivers/system.driver.rtc/rtc".to_owned(),
                        rtc.clone(),
                    ),
                    (
                        "system/drivers/system.driver.rtc/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/rtc/driver.yaml"),
                    ),
                    (
                        "system/services/system.timesvc/timesvc".to_owned(),
                        time_service.clone(),
                    ),
                    (
                        "system/services/system.timesvc/service.yaml".to_owned(),
                        time_manifest.clone(),
                    ),
                    // The PS/2 driver for the i8042 device that the
                    // server adds.
                    (
                        "system/drivers/system.driver.ps2/ps2".to_owned(),
                        ps2.clone(),
                    ),
                    (
                        "system/drivers/system.driver.ps2/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/ps2/driver.yaml"),
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
                    (
                        "system/drivers/system.driver.acpi/acpi".to_owned(),
                        acpi.clone(),
                    ),
                    (
                        "system/drivers/system.driver.acpi/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/acpi/driver.yaml"),
                    ),
                    (
                        "system/services/system.logsvc/logsvc".to_owned(),
                        log_service.clone(),
                    ),
                    (
                        "system/services/system.logsvc/service.yaml".to_owned(),
                        log_manifest.clone(),
                    ),
                    (
                        "system/drivers/system.driver.rtc/rtc".to_owned(),
                        rtc.clone(),
                    ),
                    (
                        "system/drivers/system.driver.rtc/driver.yaml".to_owned(),
                        crate::workspace_root().join("user/drivers/rtc/driver.yaml"),
                    ),
                    (
                        "system/services/system.timesvc/timesvc".to_owned(),
                        time_service.clone(),
                    ),
                    (
                        "system/services/system.timesvc/service.yaml".to_owned(),
                        time_manifest.clone(),
                    ),
                    (
                        "system/services/system.restart-probe/restart-probe".to_owned(),
                        restart_probe.clone(),
                    ),
                    (
                        "system/services/system.acpi-restart-probe/service.yaml".to_owned(),
                        crate::workspace_root()
                            .join("user/tests/restart-probe/acpi-restart-probe.yaml"),
                    ),
                ],
            )?;
            println!("built    {}", acpi_restart_image.display());
            // The input run serves this image: the ACPI server with the
            // PS/2 driver, the PCI bus, xHCI and USB HID drivers, the basic
            // display driver, the log service, through which the drivers'
            // messages reach the kernel log, and the input probe, their
            // client.
            let basic_display = user_build("basicgraphics", kind)?;
            println!("built    {}", basic_display.display());
            let input_probe = user_build("input-probe", kind)?;
            println!("built    {}", input_probe.display());
            let mut input_files: Vec<(String, PathBuf)> = Vec::new();
            for (name, binary) in [
                ("acpi", &acpi),
                ("ps2", &ps2),
                ("pcibus", &pci_bus),
                ("xhci", &xhci),
                ("usbhid", &usb_hid),
                ("basicgraphics", &basic_display),
            ] {
                input_files.extend([
                    (
                        format!("system/drivers/system.driver.{name}/{name}"),
                        binary.clone(),
                    ),
                    (
                        format!("system/drivers/system.driver.{name}/driver.yaml"),
                        crate::workspace_root().join(format!("user/drivers/{name}/driver.yaml")),
                    ),
                ]);
            }
            input_files.extend([
                (
                    "system/services/system.logsvc/logsvc".to_owned(),
                    log_service.clone(),
                ),
                (
                    "system/services/system.logsvc/service.yaml".to_owned(),
                    log_manifest.clone(),
                ),
                (
                    "system/services/system.input-probe/input-probe".to_owned(),
                    input_probe.clone(),
                ),
                (
                    "system/services/system.input-probe/service.yaml".to_owned(),
                    crate::workspace_root().join("user/tests/input-probe/input-probe.yaml"),
                ),
            ]);
            let input_image = device_image("input.img", &[], &input_files)?;
            println!("built    {}", input_image.display());
            // The console run's root partition holds these files: the
            // drivers and the log service of the input run, the terminal
            // service, the
            // temporary file system and the accounts service with the
            // test users and bob's home folder, the union service, the
            // session manager, the text console with its program manifest
            // and the libraries, keymaps and fonts it reads, and the
            // session shell as the user's shell.
            let console = crate::rust_std::build("console", kind)?;
            println!("built    {}", console.display());
            let sessions = user_build("sessionmgr", kind)?;
            println!("built    {}", sessions.display());
            let session_shell = user_build("session-shell", kind)?;
            println!("built    {}", session_shell.display());
            // The program that the session shell packs into an app.
            let app_probe = crate::rust_std::build("app-probe", kind)?;
            println!("built    {}", app_probe.display());
            // The program and libraries that it packs into a library
            // package and an app that needs them: one named by its
            // package's ID, one in the app's libraries/, and one that the
            // program finds through its DT_RUNPATH.
            let package_c = |name: &str| format!("user/tests/package-c/{name}");
            let runpath_library = c_build(
                "librunpath",
                &[&package_c("runpath.c")],
                Linking::Shared("librunpath.dlib"),
                &[],
            )?;
            let own_library = c_build(
                "libown",
                &[&package_c("own.c")],
                Linking::Shared("libown.dlib"),
                &[],
            )?;
            let greet_library = c_build(
                "greet",
                &[&package_c("greet.c")],
                Linking::Shared("org.example.greet:greet"),
                &[],
            )?;
            let package_user = c_build(
                "package-user",
                &[&package_c("main.c")],
                Linking::Dynamic,
                &[
                    runpath_library.display().to_string(),
                    own_library.display().to_string(),
                    greet_library.display().to_string(),
                    "-Wl,--enable-new-dtags,-rpath,$ORIGIN/../lib".to_owned(),
                ],
            )?;
            println!("built    {}", package_user.display());
            let idle_service = user_build("idle-service", kind)?;
            println!("built    {}", idle_service.display());
            let idle_manifest =
                crate::workspace_root().join("user/tests/idle-service/idle-service.yaml");
            let test_sessions_manifest =
                crate::workspace_root().join("user/tests/session-shell/sessionmgr.yaml");
            let mut console_files: Vec<(String, PathBuf)> = input_files
                .iter()
                .filter(|(name, _)| !name.contains("input-probe"))
                .cloned()
                .collect();
            console_files.extend([
                (
                    "system/services/system.termsvc/termsvc".to_owned(),
                    term_service.clone(),
                ),
                (
                    "system/services/system.termsvc/service.yaml".to_owned(),
                    crate::workspace_root().join("user/services/termsvc/termsvc.yaml"),
                ),
                ("system/bin/console".to_owned(), console.clone()),
                (
                    "system/libraries/ld-mikros.dlib".to_owned(),
                    dynamic_linker.clone(),
                ),
                (
                    "system/libraries/libfreetype.dlib".to_owned(),
                    freetype.install.join("libraries/libfreetype.dlib"),
                ),
                (
                    "system/libraries/libxkbcommon.dlib".to_owned(),
                    xkbcommon.install.join("libraries/libxkbcommon.dlib"),
                ),
                (
                    "system/programs/system.console.yaml".to_owned(),
                    crate::workspace_root().join("user/bin/console/system.console.yaml"),
                ),
                (
                    "system/services/system.tmpfs/tmpfs".to_owned(),
                    tmpfs.clone(),
                ),
                (
                    "system/services/system.tmpfs/service.yaml".to_owned(),
                    tmpfs_manifest.clone(),
                ),
                (
                    "system/services/system.accountsvc/accountsvc".to_owned(),
                    accounts.clone(),
                ),
                (
                    "system/services/system.accountsvc/service.yaml".to_owned(),
                    accounts_manifest.clone(),
                ),
                ("state/accounts/users".to_owned(), accounts_users(kind)?),
                (
                    "system/services/system.sessionmgr/sessionmgr".to_owned(),
                    sessions.clone(),
                ),
                (
                    "system/services/system.sessionmgr/service.yaml".to_owned(),
                    test_sessions_manifest.clone(),
                ),
                ("system/bin/msh".to_owned(), session_shell.clone()),
                ("system/bin/app-probe".to_owned(), app_probe.clone()),
                (
                    "system/share/package-c/package-user".to_owned(),
                    package_user.clone(),
                ),
                (
                    "system/share/package-c/librunpath.dlib".to_owned(),
                    runpath_library.clone(),
                ),
                (
                    "system/share/package-c/libown.dlib".to_owned(),
                    own_library.clone(),
                ),
                (
                    "system/share/package-c/greet.dlib".to_owned(),
                    greet_library.clone(),
                ),
                (
                    "system/services/system.union/union".to_owned(),
                    union.clone(),
                ),
                (
                    "system/services/system.union/service.yaml".to_owned(),
                    union_manifest.clone(),
                ),
                // The install service, which the session shell installs a
                // package for its user through.
                (
                    "system/services/system.pkgsvc/pkgsvc".to_owned(),
                    crate::rust_std::build("pkgsvc", kind)?,
                ),
                (
                    "system/services/system.pkgsvc/service.yaml".to_owned(),
                    crate::workspace_root().join("user/services/pkgsvc/pkgsvc.yaml"),
                ),
                (
                    "apps/README".to_owned(),
                    crate::workspace_root().join("user/tests/session-shell/system/Apps/README"),
                ),
                (
                    "libraries/README".to_owned(),
                    crate::workspace_root()
                        .join("user/tests/session-shell/system/Libraries/README"),
                ),
                (
                    "users/bob/libraries/README".to_owned(),
                    crate::workspace_root().join("user/tests/session-shell/home/Libraries/README"),
                ),
            ]);
            for font in ["DejaVuSansMono.ttf", "DejaVuSansMono-Bold.ttf"] {
                console_files.push((
                    format!("system/share/fonts/dejavu/{font}"),
                    fonts.ttf.join(font),
                ));
            }
            let keymap_data = keymaps.install.join("share/xkeyboard-config-2");
            let mut keymap_files = Vec::new();
            crate::port_tests::collect(&keymap_data, &mut keymap_files)?;
            for path in keymap_files {
                let relative = path
                    .strip_prefix(&keymap_data)?
                    .to_str()
                    .context("not UTF-8")?;
                console_files.push((format!("system/share/X11/xkb/{relative}"), path.clone()));
            }
            // The power-off run serves this image, with the ACPI server, the
            // log service and the power probe, a boot service that the
            // service manager stops when the power button shuts the
            // machine down, and the PCI bus driver with the drivers of the
            // disk controllers, of the xHCI controller, of USB mass storage
            // and of partitions, whose devices the device manager removes
            // when it stops.
            let mut power_files: Vec<(String, PathBuf)> = vec![
                (
                    "system/drivers/system.driver.acpi/acpi".to_owned(),
                    acpi.clone(),
                ),
                (
                    "system/drivers/system.driver.acpi/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/acpi/driver.yaml"),
                ),
            ];
            for (name, binary) in [
                ("pcibus", &pci_bus),
                ("virtioblk", &virtio_blk),
                ("nvme", &nvme),
                ("ahci", &ahci),
                ("xhci", &xhci),
                ("usbstorage", &usb_storage),
                ("partition", &partition),
            ] {
                let manifest = format!("user/drivers/{name}/driver.yaml");
                power_files.extend([
                    (
                        format!("system/drivers/system.driver.{name}/{name}"),
                        binary.clone(),
                    ),
                    (
                        format!("system/drivers/system.driver.{name}/driver.yaml"),
                        crate::workspace_root().join(manifest),
                    ),
                ]);
            }
            power_files.extend([
                // The log service, through which the server's messages
                // reach the kernel log.
                (
                    "system/services/system.logsvc/logsvc".to_owned(),
                    log_service.clone(),
                ),
                (
                    "system/services/system.logsvc/service.yaml".to_owned(),
                    log_manifest.clone(),
                ),
                (
                    "system/services/system.power-probe/power-probe".to_owned(),
                    power_probe.clone(),
                ),
                (
                    "system/services/system.power-probe/service.yaml".to_owned(),
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
            // that checks it, and one for each scratch partition, and
            // tmpfs.
            let pci_bus_files = [
                (
                    "system/drivers/system.driver.pcibus/pcibus".to_owned(),
                    pci_bus.clone(),
                ),
                (
                    "system/drivers/system.driver.pcibus/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/pcibus/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.virtioblk/virtioblk".to_owned(),
                    virtio_blk.clone(),
                ),
                (
                    "system/drivers/system.driver.virtioblk/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/virtioblk/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.nvme/nvme".to_owned(),
                    nvme.clone(),
                ),
                (
                    "system/drivers/system.driver.nvme/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/nvme/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.ahci/ahci".to_owned(),
                    ahci.clone(),
                ),
                (
                    "system/drivers/system.driver.ahci/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/ahci/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.xhci/xhci".to_owned(),
                    xhci.clone(),
                ),
                (
                    "system/drivers/system.driver.xhci/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/xhci/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.usbstorage/usbstorage".to_owned(),
                    usb_storage.clone(),
                ),
                (
                    "system/drivers/system.driver.usbstorage/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/usbstorage/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.partition/partition".to_owned(),
                    partition.clone(),
                ),
                (
                    "system/drivers/system.driver.partition/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/partition/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.fat/fat".to_owned(),
                    fat.clone(),
                ),
                (
                    "system/drivers/system.driver.fat/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/fat/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.exfat/exfat".to_owned(),
                    exfat.clone(),
                ),
                (
                    "system/drivers/system.driver.exfat/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/exfat/driver.yaml"),
                ),
                (
                    "system/drivers/system.driver.btrfs/btrfs".to_owned(),
                    btrfs.clone(),
                ),
                (
                    "system/drivers/system.driver.btrfs/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/btrfs/driver.yaml"),
                ),
                // The log service, which keeps the log in files on the
                // root file system and the EFI system partition.
                (
                    "system/services/system.logsvc/logsvc".to_owned(),
                    log_service.clone(),
                ),
                (
                    "system/services/system.logsvc/service.yaml".to_owned(),
                    log_manifest.clone(),
                ),
                // The time service, which takes the time zone from the
                // root file system.
                (
                    "system/services/system.timesvc/timesvc".to_owned(),
                    time_service.clone(),
                ),
                (
                    "system/services/system.timesvc/service.yaml".to_owned(),
                    time_manifest.clone(),
                ),
                // The temporary file system, in which the drivers that
                // start once the root file system is available get their
                // folders.
                (
                    "system/services/system.tmpfs/tmpfs".to_owned(),
                    tmpfs.to_path_buf(),
                ),
                (
                    "system/services/system.tmpfs/service.yaml".to_owned(),
                    tmpfs_manifest.to_path_buf(),
                ),
                // The USB HID driver, for the keyboard behind the hub.
                (
                    "system/drivers/system.driver.usbhid/usbhid".to_owned(),
                    usb_hid.clone(),
                ),
                (
                    "system/drivers/system.driver.usbhid/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/usbhid/driver.yaml"),
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
            // The USB volumes run's root partition holds these files: the
            // PCI bus, xHCI, USB mass storage, partition, FAT, exFAT and
            // btrfs drivers, the time zones and the log service, through
            // which the drivers' messages reach the serial output, from the
            // PCI test's files, the temporary file system, and the volumes
            // probe.
            let volumes_probe = user_build("volumes-probe", kind)?;
            println!("built    {}", volumes_probe.display());
            // The volumes utility, and a C program that checks the paths
            // that realpath gives below /volumes, which the probe runs.
            let volumes_utility = crate::rust_std::build("volumes", kind)?;
            println!("built    {}", volumes_utility.display());
            let volume_path = c_build(
                "volume-path",
                &["user/tests/volume-path/main.c"],
                Linking::Dynamic,
                &[],
            )?;
            println!("built    {}", volume_path.display());
            let volume_drivers = [
                "pcibus",
                "xhci",
                "usbstorage",
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
                        .any(|d| at.starts_with(&format!("system/drivers/system.driver.{d}/")))
                        || at.starts_with("system/share/zoneinfo/")
                        || at.contains("logsvc")
                })
                .cloned()
                .collect();
            volumes_files.extend([
                (
                    "system/services/system.tmpfs/tmpfs".to_owned(),
                    tmpfs.clone(),
                ),
                (
                    "system/services/system.tmpfs/service.yaml".to_owned(),
                    tmpfs_manifest.clone(),
                ),
                (
                    "system/services/system.volumes-probe/volumes-probe".to_owned(),
                    volumes_probe.clone(),
                ),
                ("system/bin/volumes".to_owned(), volumes_utility),
                ("system/bin/volume-path".to_owned(), volume_path),
                (
                    "system/libraries/ld-mikros.dlib".to_owned(),
                    dynamic_linker.clone(),
                ),
                (
                    "system/services/system.volumes-probe/service.yaml".to_owned(),
                    crate::workspace_root().join("user/tests/volumes-probe/volumes-probe.yaml"),
                ),
            ]);
            // The root disk runs serve this image: the PCI bus driver, the
            // drivers of the disk controllers, the partition, FAT and btrfs
            // drivers, the time zones, the log service and tmpfs, from the
            // PCI test's files, without test drivers.
            let root_drivers = [
                "pcibus",
                "virtioblk",
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
                        .any(|d| at.starts_with(&format!("system/drivers/system.driver.{d}/")))
                        || at.starts_with("system/share/zoneinfo/")
                        || at.contains("logsvc")
                        || at.contains("system.tmpfs/")
                })
                .cloned()
                .collect();
            let root_image = device_image("root.img", &[], &root_files)?;
            println!("built    {}", root_image.display());
            // The root partitions of the namespaces, accounts, console and
            // USB volumes runs hold the files of these archives, whose
            // services start once the root file system is available; the
            // test init serves setup.img for them.
            let archive = |file: &str, files: &[(String, PathBuf)]| {
                let members: Vec<(&str, &Path)> = files
                    .iter()
                    .map(|(name, path)| (name.as_str(), path.as_path()))
                    .collect();
                test_image(kind, file, &members)
            };
            // The live run's root partition holds the probe of a live
            // system's namespace, with its manifest, and the file of
            // /system/config that it reads and writes over.
            let live_probe = user_build("live-probe", kind)?;
            println!("built    {}", live_probe.display());
            let live_dir = crate::workspace_root().join("user/tests/live-probe");
            let live_conf = live_dir.join("live-probe.conf");
            let live_files: Vec<(String, PathBuf)> = vec![
                (
                    "system/services/system.live-probe/live-probe".to_owned(),
                    live_probe,
                ),
                (
                    "system/services/system.live-probe/service.yaml".to_owned(),
                    live_dir.join("live-probe.yaml"),
                ),
                (
                    "system/share/live-probe/live-probe.conf".to_owned(),
                    live_conf.clone(),
                ),
                ("config/live-probe.conf".to_owned(), live_conf),
            ];
            for (file, files) in [
                ("live.img", &live_files),
                ("namespaces.img", &namespaces_files),
                ("accounts.img", &accounts_files),
                ("console.img", &console_files),
                ("volumes.img", &volumes_files),
            ] {
                println!("built    {}", archive(file, files)?.display());
            }
            // The setup run serves this image: the drivers and the log
            // service of the input run, the root disk's drivers and tmpfs
            // of the root disk runs, and the union server, the services
            // that run until the root file system is available.
            // The rest of the system comes from the root partition, which
            // the run makes from the first-boot archive below.
            let mut setup_files: Vec<(String, PathBuf)> = input_files
                .iter()
                .filter(|(name, _)| !name.contains("input-probe"))
                .cloned()
                .collect();
            setup_files.extend(
                root_files
                    .iter()
                    .filter(|(at, _)| !setup_files.iter().any(|(have, _)| have == at))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            setup_files.extend([
                (
                    "system/services/system.timesvc/timesvc".to_owned(),
                    time_service.clone(),
                ),
                (
                    "system/services/system.timesvc/service.yaml".to_owned(),
                    time_manifest.clone(),
                ),
                (
                    "system/drivers/system.driver.rtc/rtc".to_owned(),
                    rtc.clone(),
                ),
                (
                    "system/drivers/system.driver.rtc/driver.yaml".to_owned(),
                    crate::workspace_root().join("user/drivers/rtc/driver.yaml"),
                ),
                (
                    "system/services/system.union/union".to_owned(),
                    union.clone(),
                ),
                (
                    "system/services/system.union/service.yaml".to_owned(),
                    union_manifest.clone(),
                ),
            ]);
            let setup_image = device_image("setup.img", &[], &setup_files)?;
            println!("built    {}", setup_image.display());
            // The bus restart runs serve this image: the root disk runs'
            // with the restart probe.
            let mut bus_files = root_files.clone();
            bus_files.extend([
                (
                    "system/services/system.restart-probe/restart-probe".to_owned(),
                    restart_probe.clone(),
                ),
                (
                    "system/services/system.restart-probe/service.yaml".to_owned(),
                    crate::workspace_root().join("user/tests/restart-probe/restart-probe.yaml"),
                ),
            ]);
            let bus_image = device_image("bus.img", &[], &bus_files)?;
            println!("built    {}", bus_image.display());
            // The durability run serves this image: the PCI bus driver, the
            // drivers of the disk controllers, of USB mass storage and of
            // partitions, the log service and tmpfs, from the PCI test's
            // files, with the durability test driver.
            let durable_drivers = [
                "pcibus",
                "virtioblk",
                "nvme",
                "ahci",
                "xhci",
                "usbstorage",
                "partition",
            ];
            let durable_files: Vec<(String, PathBuf)> = pci_bus_files
                .iter()
                .filter(|(at, _)| {
                    durable_drivers
                        .iter()
                        .any(|d| at.starts_with(&format!("system/drivers/system.driver.{d}/")))
                        || at.contains("logsvc")
                        || at.contains("system.tmpfs/")
                })
                .cloned()
                .collect();
            let durable_image =
                device_image("durable.img", &[("test-durable", true)], &durable_files)?;
            println!("built    {}", durable_image.display());
            let members = [
                ("tests/broker.img", broker_image.as_path()),
                ("tests/services.img", services_image.as_path()),
                ("tests/tmpfs.img", tmpfs_image.as_path()),
                ("tests/log.img", log_image.as_path()),
                ("tests/term.img", term_image.as_path()),
                ("tests/time.img", time_image.as_path()),
                ("tests/devices.img", devices_image.as_path()),
                ("tests/failing.img", failing_image.as_path()),
                ("tests/restart.img", restart_image.as_path()),
                ("tests/acpi.img", acpi_image.as_path()),
                ("tests/power.img", power_image.as_path()),
                ("tests/pci.img", pci_image.as_path()),
                ("tests/root.img", root_image.as_path()),
                ("tests/bus.img", bus_image.as_path()),
                ("tests/durable.img", durable_image.as_path()),
                ("tests/acpi-restart.img", acpi_restart_image.as_path()),
                ("tests/input.img", input_image.as_path()),
                ("tests/setup.img", setup_image.as_path()),
                ("init", init.as_path()),
                ("tests/launcher.img", launcher.as_path()),
                ("tests/ports.img", ports_image.as_path()),
                ("tests/channel-echo", echo.as_path()),
                ("tests/call-server", server.as_path()),
                ("tests/debuggee", debuggee.as_path()),
                ("tests/pie", pie.as_path()),
                ("tests/managed", managed.as_path()),
                ("system/processmgr", manager.as_path()),
                ("system/servicemgr", services.as_path()),
                // A dynamically linked program with its libraries and the
                // dynamic linker, which the test machine's root partition
                // gets with the rest of the system version, and the nested systems' boot
                // images lack: the bus restart runs start it from the root.
                ("system/bin/library-user", library_user.as_path()),
                ("system/libraries/libgreet.1.dlib", greet.as_path()),
                ("system/libraries/libnonote.1.dlib", no_note.as_path()),
                ("system/libraries/ld-mikros.dlib", dynamic_linker.as_path()),
            ];
            // The time zones, which the test machine's root partition
            // gets with the rest of the system version.
            let zones = crate::tzdata::members(&crate::tzdata::zoneinfo()?)?;
            let members: Vec<(&str, &Path)> = members
                .into_iter()
                .chain(zones.iter().map(|(at, path)| (at.as_str(), path.as_path())))
                .collect();
            let image = boot_image(kind, "test-boot.img", &members)?;
            // The root partition of the setup run gets this archive: the
            // test boot image with the services that need the root file
            // system and what they load, msh among them, the session
            // manager's manifest with the kernel log handle,
            // spawn-std, which runs as the job that the runner stops in
            // msh, files-c, which ada's msh runs in her home, and the idle
            // service, which ada's msh stops and starts, global-check,
            // which installs packages for every user, and the program and
            // libraries of package-c that it packs. The dynamic linker is
            // in both lists, and goes in once.
            let sessions_manifest = "system/services/system.sessionmgr/service.yaml";
            let console_fonts =
                ["DejaVuSansMono.ttf", "DejaVuSansMono-Bold.ttf"].map(|f| fonts.ttf.join(f));
            let login = login_members(
                kind,
                &xkbcommon,
                &freetype,
                &keymaps,
                &wayland.library,
                &vulkan_members(&vulkan, &mesa, &llvm),
            )?;
            let first_boot: Vec<(&str, &Path)> = members
                .iter()
                .copied()
                .chain(
                    login
                        .iter()
                        .filter(|(at, _)| at != sessions_manifest)
                        .filter(|(at, _)| !members.iter().any(|(m, _)| m == at))
                        .map(|(at, path)| (at.as_str(), path.as_path())),
                )
                .chain([
                    (sessions_manifest, test_sessions_manifest.as_path()),
                    ("system/bin/spawn-std", spawn_std.as_path()),
                    ("system/bin/files-c", files_c.as_path()),
                    ("system/bin/global-check", global_check.as_path()),
                    ("system/bin/forge-probe", forge_probe.as_path()),
                    (
                        "system/programs/system.forge-probe.yaml",
                        forge_manifest.as_path(),
                    ),
                    (
                        "system/share/package-c/package-user",
                        package_user.as_path(),
                    ),
                    (
                        "system/share/package-c/librunpath.dlib",
                        runpath_library.as_path(),
                    ),
                    ("system/share/package-c/libown.dlib", own_library.as_path()),
                    ("system/share/package-c/greet.dlib", greet_library.as_path()),
                    (
                        "system/services/system.idle-service/idle-service",
                        idle_service.as_path(),
                    ),
                    (
                        "system/services/system.idle-service/service.yaml",
                        idle_manifest.as_path(),
                    ),
                    (
                        "system/share/fonts/dejavu/DejaVuSansMono.ttf",
                        console_fonts[0].as_path(),
                    ),
                    (
                        "system/share/fonts/dejavu/DejaVuSansMono-Bold.ttf",
                        console_fonts[1].as_path(),
                    ),
                ])
                .collect();
            let first_boot = boot_image(kind, "first-boot.img", &first_boot)?;
            println!("built    {}", first_boot.display());
            first_boot_image = Some(first_boot);
            // The takeover run's root partition: the test boot image with
            // a normal driver for QEMU's VGA function, which the root alone
            // holds.
            let takeover_manifest =
                crate::workspace_root().join("user/tests/drivers/test-takeover/driver.yaml");
            let takeover: Vec<(&str, &Path)> = members
                .iter()
                .copied()
                .chain([
                    (
                        "system/drivers/system.driver.test-takeover/driver.yaml",
                        takeover_manifest.as_path(),
                    ),
                    (
                        "system/drivers/system.driver.test-takeover/test-takeover",
                        test_driver.as_path(),
                    ),
                ])
                .collect();
            let takeover = boot_image(kind, "takeover.img", &takeover)?;
            println!("built    {}", takeover.display());
            takeover_image = Some(takeover);
            (image.clone(), image)
        }
    };
    println!("built    {}", boot_image.display());
    Ok(Built {
        bootmenu,
        bootloader,
        kernel,
        boot_image,
        system_image,
        first_boot_image,
        takeover_image,
        desktop_image,
        dawn_image,
        dawn_desktop_image,
        xkbcommon,
    })
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

/// Writes the users file of the desktop run for `kind` and returns its
/// path: `dee`, an administrator whose login mode is `CONSOLE`, with the
/// password `dee-password`, which the greeter probe logs in. A fixed salt
/// keeps the file the same from one build to the next.
fn desktop_users(kind: Kind) -> Result<PathBuf> {
    use mikros_userdb::{Kind as UserKind, LoginMode, Users, encode, hash};
    let mut users = Users::new();
    let salt = [7; hash::SALT_LEN];
    let phc = hash::hash(b"dee-password", &salt).context("hashing a password")?;
    let id = users
        .create("dee", "Dee", UserKind::Admin, phc)
        .map_err(|r| anyhow::anyhow!("adding dee: {r:?}"))?;
    users
        .set_login_mode(id, LoginMode::Console)
        .map_err(|r| anyhow::anyhow!("setting the login mode of dee: {r:?}"))?;
    let dir = crate::workspace_root()
        .join("target/boot")
        .join(kind.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join("desktop-users");
    fs::write(&path, encode(&users)).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The files of Vulkan that the system image holds: the loader `loader`,
/// lavapipe, the CPU driver, from `mesa`, with its manifest where
/// drivers' manifests go, and LLVM's library from `llvm`, which it needs.
fn vulkan_members(loader: &Path, mesa: &Path, llvm: &Path) -> Vec<(String, PathBuf)> {
    vec![
        (
            format!("system/libraries/{}", crate::vulkan::LOADER),
            loader.join("libraries").join(crate::vulkan::LOADER),
        ),
        (
            format!("system/libraries/{}", crate::mesa::DRIVER),
            mesa.join("libraries").join(crate::mesa::DRIVER),
        ),
        (
            format!("system/{}", crate::mesa::MANIFEST),
            mesa.join(crate::mesa::MANIFEST),
        ),
        (
            "system/libraries/libLLVM.dlib".into(),
            llvm.join("lib/libLLVM.dlib"),
        ),
    ]
}

/// The members of the system image that logging in on the text console
/// needs: the components that only the system image holds, among them
/// the accounts, terminal and session services, the text console, the
/// setup program and the user's shell msh, then the command-line
/// utilities, the dynamic linker, libraries and keymaps they load, and the
/// Wayland client library.
/// The fonts and the basic display driver, which the device manager
/// must find in the boot image, are among the other members.
fn login_members(
    kind: Kind,
    xkbcommon: &crate::ports::Built,
    freetype: &crate::ports::Built,
    keymaps: &crate::ports::Built,
    wayland: &Path,
    vulkan: &[(String, PathBuf)],
) -> Result<Vec<(String, PathBuf)>> {
    let mut members = Vec::new();
    for component in crate::components::all()? {
        if component.image == Image::System && component.in_kind(kind) {
            let program = component_build(&component, kind)?;
            members.extend(component.members(kind, &program)?);
        }
    }
    members.extend([
        (
            "system/libraries/ld-mikros.dlib".into(),
            crate::sysroot::dir().join("libraries/ld-mikros.dlib"),
        ),
        (
            "system/libraries/libfreetype.dlib".into(),
            freetype.install.join("libraries/libfreetype.dlib"),
        ),
        (
            "system/libraries/libxkbcommon.dlib".into(),
            xkbcommon.install.join("libraries/libxkbcommon.dlib"),
        ),
        (
            format!("system/libraries/{}", crate::wayland::LIBRARY),
            wayland.to_path_buf(),
        ),
    ]);
    // The C++ runtimes, shared.
    let lib = crate::sysroot::dir().join("libraries");
    members.extend([
        (
            "system/libraries/libc++.dlib".into(),
            lib.join("libc++.dlib"),
        ),
        (
            "system/libraries/libc++abi.dlib".into(),
            lib.join("libc++abi.dlib"),
        ),
        (
            "system/libraries/libunwind.dlib".into(),
            lib.join("libunwind.dlib"),
        ),
    ]);
    members.extend_from_slice(vulkan);
    let keymap_data = keymaps.install.join("share/xkeyboard-config-2");
    let mut keymap_files = Vec::new();
    crate::port_tests::collect(&keymap_data, &mut keymap_files)?;
    for path in keymap_files {
        let relative = path
            .strip_prefix(&keymap_data)?
            .to_str()
            .context("not UTF-8")?;
        members.push((format!("system/share/X11/xkb/{relative}"), path.clone()));
    }
    Ok(members)
}

/// Writes the boot archive `file` with each of `members`, given by name
/// and the path of its contents, as a regular file.
/// Where the archive `file` of the build kind `kind` is written, the test
/// images that the test boot image holds among them.
pub fn test_image_path(kind: Kind, file: &str) -> PathBuf {
    crate::workspace_root()
        .join("target/boot")
        .join(kind.name())
        .join(file)
}

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
pub(crate) fn without_debug_info(kind: Kind, source: &Path) -> Result<PathBuf> {
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
/// Beside the archive, `<file>.list` lists its members (see `Listing`).
fn archive(
    kind: Kind,
    file: &str,
    members: &[(&str, &Path)],
    dirs: &[&str],
    links: &[(&str, &str)],
) -> Result<PathBuf> {
    let path = test_image_path(kind, file);
    fs::create_dir_all(path.parent().context("an archive in a directory")?)?;
    let mut listing = Listing::default();
    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut archive = tar::Builder::new(file);
    for &(name, source) in members {
        let data = fs::read(source).with_context(|| format!("reading {}", source.display()))?;
        listing.add(name, format!("file 755 {} {}", data.len(), sha256(&data)))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        archive
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("adding {name} to the boot archive"))?;
    }
    for &dir in dirs {
        listing.add(&dir_name(dir), "dir 755".to_owned())?;
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        archive
            .append_data(&mut header, dir_name(dir), std::io::empty())
            .with_context(|| format!("adding {dir} to the boot archive"))?;
    }
    for &(name, target) in links {
        listing.add(name, format!("link 777 {target}"))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(tar::EntryType::Symlink);
        archive
            .append_link(&mut header, name, target)
            .with_context(|| format!("adding {name} to the boot archive"))?;
    }
    archive.finish().context("finishing the boot archive")?;
    listing.write(&path)?;
    Ok(path)
}

/// The members of an archive, one line each, sorted by name: the name,
/// its type, mode and, for files, the size and the SHA-256 of the
/// contents. Two builds' images can be compared by their listings.
#[derive(Default)]
struct Listing(std::collections::BTreeMap<String, String>);

impl Listing {
    /// Adds the member `name`, which must not be in the archive yet.
    fn add(&mut self, name: &str, line: String) -> Result<()> {
        if self.0.insert(name.to_owned(), line).is_some() {
            bail!("{name} is in the archive twice");
        }
        Ok(())
    }

    /// Writes the listing beside the archive at `archive`, as
    /// `<archive>.list`.
    fn write(&self, archive: &Path) -> Result<()> {
        let mut path = archive.as_os_str().to_owned();
        path.push(".list");
        let text: String = self.0.iter().map(|(n, l)| format!("{n} {l}\n")).collect();
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }
}

fn sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    crate::fetch::hex(&Sha256::digest(data))
}

/// The archive name of directory `dir`, which ends in a slash.
fn dir_name(dir: &str) -> String {
    format!("{dir}/")
}

pub fn run(args: &Args) -> Result<()> {
    build_edition(args.kind, args.edition).map(drop)
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

/// Builds the program of `component`, with the library that its build
/// links first for the ACPI and btrfs drivers, and returns its path.
fn component_build(component: &Component, kind: Kind) -> Result<PathBuf> {
    match (component.name.as_str(), component.runtime) {
        ("acpi", _) => acpi_build(kind),
        ("btrfs", _) => btrfs_build(kind),
        (name, Runtime::Rt) => {
            let program = user_build(name, kind)?;
            println!("built    {}", program.display());
            Ok(program)
        }
        (name, Runtime::Std) => {
            let program = crate::rust_std::build(name, kind)?;
            println!("built    {}", program.display());
            Ok(program)
        }
    }
}

/// The user-space programs that `prebuild` built, by build kind and
/// name.
static PREBUILT: Mutex<BTreeSet<(&str, String)>> = Mutex::new(BTreeSet::new());

/// Builds the user-space programs `bins`, packages of the `user`
/// workspace of the same names, in one run of Cargo, which compiles
/// their crates in parallel, after the libraries that the ACPI and btrfs
/// drivers link. `user_build` then finds them built.
fn prebuild(bins: &[String], kind: Kind) -> Result<()> {
    if bins.iter().any(|b| b == "acpi") {
        crate::acpica::build(crate::acpica::Flavor::Mikros)?;
    }
    if bins.iter().any(|b| b == "btrfs") {
        crate::btrfs::build(crate::btrfs::Flavor::Mikros)?;
    }
    let mut command = user_cargo(kind);
    for bin in bins {
        command.args(["--package", bin]);
    }
    let status = command
        .arg("--bins")
        .status()
        .context("failed to run cargo")?;
    if !status.success() {
        bail!("building the user-space programs failed");
    }
    let mut prebuilt = PREBUILT.lock().expect("not poisoned");
    prebuilt.extend(bins.iter().map(|b| (kind.name(), b.clone())));
    Ok(())
}

/// Sets Cargo up in `command` for programs without std for the Mikros
/// target, whose core and alloc libraries it builds from source, with
/// the memory functions of compiler_builtins, and which abort on panic
/// in every profile.
pub fn without_std(command: &mut Command) -> &mut Command {
    command
        .env("CARGO_UNSTABLE_BUILD_STD", "core,alloc,compiler_builtins")
        .env("CARGO_UNSTABLE_BUILD_STD_FEATURES", "compiler-builtins-mem")
        .env("CARGO_UNSTABLE_JSON_TARGET_SPEC", "true");
    for profile in ["DEV", "RELEASE", "FUZZ", "DIAG"] {
        command.env(format!("CARGO_PROFILE_{profile}_PANIC"), "abort");
    }
    command
}

/// `cargo build` in the `user` workspace for the build kind `kind`.
fn user_cargo(kind: Kind) -> Command {
    let root = crate::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut command = Command::new(cargo);
    without_std(&mut command)
        .current_dir(root.join("user"))
        .args(["build", "--target"])
        .arg(root.join(USER_TARGET))
        .args(["--profile", kind.profile()]);
    let flags = kind.user_rustflags();
    if !flags.is_empty() {
        let flags: Vec<String> = flags.iter().map(|f| format!("\"{f}\"")).collect();
        command
            .arg("--config")
            .arg(format!("build.rustflags=[{}]", flags.join(", ")));
    }
    command
}

/// Builds the user-space program `bin`, a package of the `user`
/// workspace of the same name, unless `prebuild` did, and returns its
/// path.
fn user_build(bin: &str, kind: Kind) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let path = root
        .join("user/target/x86_64-unknown-mikros")
        .join(kind.profile_dir())
        .join(bin);
    let prebuilt = PREBUILT.lock().expect("not poisoned");
    if prebuilt.contains(&(kind.name(), bin.to_owned())) {
        return Ok(path);
    }
    drop(prebuilt);
    let mut command = user_cargo(kind);
    command.args(["--package", bin, "--bin", bin]);
    // The test init of a fuzz build also tests the fuzz-only calls.
    if matches!(kind, Kind::Fuzz) && bin == "test-init" {
        command.args(["--features", "fuzz"]);
    }
    let status = command.status().context("failed to run cargo")?;
    if !status.success() {
        bail!("building {bin} failed");
    }
    Ok(path)
}

/// How a C program is linked with the C library.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Linking {
    Static,
    Dynamic,
    /// A shared library `<name>.dlib` with this `DT_SONAME`.
    Shared(&'static str),
}

/// Builds the C program or library `name` from `sources`, paths relative
/// to the repository, against the sysroot, with the further arguments
/// `args`, and returns its path, in `target/c/<name>/`.
pub(crate) fn c_build(
    name: &str,
    sources: &[&str],
    linking: Linking,
    args: &[String],
) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let sysroot = crate::sysroot::build()?;
    let dir = c_dir(name);
    fs::create_dir_all(&dir)?;
    // C++ sources are built and linked as C++, with the C++ runtimes.
    let cxx = sources.iter().any(|s| s.ends_with(".cpp"));
    let compiler = if cxx { "clang++" } else { "clang" };
    let mut command = Command::new(crate::llvm::tool(compiler));
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

/// The members of the system apps of the desktop run's greeter and of
/// its user session's shell, both the C program of
/// `user/tests/greeter-probe`, with the client code of
/// xdg-shell that the build host's `wayland-scanner` of `wayland`
/// generates for it, linked with libwayland and the Vulkan loader in
/// `vulkan`, whose headers are in `headers`.
fn greeter_probe(
    wayland: &crate::wayland::Built,
    vulkan: &Path,
    headers: &Path,
    sdl: &Path,
    xkbcommon: &crate::ports::Built,
) -> Result<Vec<(String, PathBuf)>> {
    let name = "greeter-probe";
    let dir = c_dir(name);
    fs::create_dir_all(&dir)?;
    // The client code of xdg-shell, and of the compositor's buffer and
    // shell protocols, with that of xdg-activation, which the shell's
    // names.
    let staged = crate::wayland::protocols()?;
    let own = crate::workspace_root().join("user/bin/compositor/protocols");
    crate::wayland::install_protocols()?;
    let tablet = crate::sysroot::dir().join("share/wayland-protocols/stable/tablet/tablet-v2.xml");
    let mut code = Vec::new();
    for (xml, stem) in [
        (staged.join("xdg-shell.xml"), "xdg-shell"),
        (own.join("mikros-buffer-v1.xml"), "mikros-buffer-v1"),
        (staged.join("xdg-activation-v1.xml"), "xdg-activation-v1"),
        (own.join("mikros-shell-v1.xml"), "mikros-shell-v1"),
        (own.join("mikros-material-v1.xml"), "mikros-material-v1"),
        (staged.join("cursor-shape-v1.xml"), "cursor-shape-v1"),
        // The protocols of the protocols client.
        (staged.join("viewporter.xml"), "viewporter"),
        (
            staged.join("fractional-scale-v1.xml"),
            "fractional-scale-v1",
        ),
        (staged.join("presentation-time.xml"), "presentation-time"),
        (
            staged.join("keyboard-shortcuts-inhibit-unstable-v1.xml"),
            "keyboard-shortcuts-inhibit-unstable-v1",
        ),
        (
            staged.join("wlr-layer-shell-unstable-v1.xml"),
            "wlr-layer-shell-unstable-v1",
        ),
        // Only for the tablet tool's interface, which cursor-shape names.
        (tablet.clone(), "tablet-v2"),
    ] {
        let source = dir.join(format!("{stem}-protocol.c"));
        for (mode, out) in [
            (
                "client-header",
                dir.join(format!("{stem}-client-protocol.h")),
            ),
            ("private-code", source.clone()),
        ] {
            crate::port::run(
                Command::new(wayland.scanner_program())
                    .arg(mode)
                    .arg(&xml)
                    .arg(out),
            )
            .with_context(|| format!("generating the {stem} code of greeter-probe"))?;
        }
        code.push(source.display().to_string());
    }
    let args = [
        "-Wall".to_string(),
        "-Wextra".into(),
        "-Werror".into(),
        format!("-I{}", headers.display()),
        format!("-I{}", dir.display()),
        wayland.library.display().to_string(),
        vulkan
            .join("libraries")
            .join(crate::vulkan::LOADER)
            .display()
            .to_string(),
    ];
    let mut sources = vec![
        "user/tests/greeter-probe/greeter.c",
        "user/tests/greeter-probe/wallpaper.c",
        "user/tests/greeter-probe/panel.c",
        "user/tests/greeter-probe/window.c",
        "user/tests/greeter-probe/protocols.c",
        "user/tests/greeter-probe/protocols_input.c",
    ];
    sources.extend(code.iter().map(String::as_str));
    let program = c_build(name, &sources, Linking::Dynamic, &args)?;
    println!("built    {}", program.display());
    let sdl_program = sdl_probe(wayland, vulkan, headers, sdl, xkbcommon)?;
    let greeter = crate::system_apps::SystemApp {
        id: "system.mikros.greeter_probe",
        version: "1.0.0",
        name: "Greeter probe",
        description: "The greeter of the desktop run",
        executable: name,
        shell_parts: "  - roles: [greeter]\n    executable: greeter-probe\n    \
                      arguments: [\"--login\", dee, dee-password]\n",
        files: vec![(name.to_owned(), program.clone(), true)],
    };
    let parts = shell_probe_parts("sdl-probe");
    let shell = shell_probe(&parts, program, "sdl-probe", sdl_program);
    let mut members = crate::system_apps::members(&greeter, &dir.join("app"))?;
    members.extend(crate::system_apps::members(&shell, &dir.join("app"))?);
    Ok(members)
}

/// The ID of the shell probe, the system app that is the shell of the
/// desktop run's user session.
const SHELL_PROBE: &str = "system.mikros.shell_probe";

/// The name of the switcher part's program in the Dawn run's shell probe.
const DAWN_SWITCHER: &str = "dawn-sdl";

/// The `shell_parts` of the shell probe: its desktop part hands the
/// compositor a wallpaper of a test colour, its taskbar part shows a panel
/// of haze, its start-menu part connects to the service manager's Control
/// and so asks a question, and its switcher part is the program
/// `switcher`, a window that only has to show.
fn shell_probe_parts(switcher: &str) -> String {
    format!(
        "  - roles: [desktop]\n    executable: greeter-probe\n    \
         arguments: [\"--wallpaper\"]\n  \
         - roles: [taskbar]\n    executable: greeter-probe\n    \
         arguments: [\"--panel\"]\n  \
         - roles: [start-menu]\n    executable: greeter-probe\n    \
         arguments: [\"--control\"]\n  \
         - roles: [switcher]\n    executable: {switcher}\n    \
         arguments: []\n"
    )
}

/// The shell probe with the greeter probe `greeter` for its desktop,
/// taskbar and start-menu parts and the program `switcher`, which is
/// installed as `executable`, for its switcher part; `parts` is
/// `shell_probe_parts(executable)`.
fn shell_probe<'a>(
    parts: &'a str,
    greeter: PathBuf,
    executable: &str,
    switcher: PathBuf,
) -> crate::system_apps::SystemApp<'a> {
    crate::system_apps::SystemApp {
        id: SHELL_PROBE,
        version: "1.0.0",
        name: "Shell probe",
        description: "The shell of the desktop run",
        executable: "greeter-probe",
        shell_parts: parts,
        files: vec![
            ("greeter-probe".to_owned(), greeter, true),
            (executable.to_owned(), switcher, true),
        ],
    }
}

/// Builds the SDL part of the desktop run, `user/tests/sdl-probe`: a
/// window of SDL's Wayland driver that Vulkan clears and presents. SDL3 is
/// linked statically, from `sdl`'s install, and the libraries that it
/// needs, Wayland and xkbcommon, and the Vulkan loader dynamically.
fn sdl_probe(
    wayland: &crate::wayland::Built,
    vulkan: &Path,
    headers: &Path,
    sdl: &Path,
    xkbcommon: &crate::ports::Built,
) -> Result<PathBuf> {
    let args = [
        "-Wall".to_string(),
        "-Wextra".into(),
        "-Werror".into(),
        format!("-I{}", headers.display()),
        format!("-I{}", sdl.join("include").display()),
        sdl.join("libraries")
            .join(crate::sdl::LIBRARY)
            .display()
            .to_string(),
        wayland.library.display().to_string(),
        xkbcommon
            .install
            .join("libraries/libxkbcommon.dlib")
            .display()
            .to_string(),
        vulkan
            .join("libraries")
            .join(crate::vulkan::LOADER)
            .display()
            .to_string(),
    ];
    let program = c_build(
        "sdl-probe",
        &["user/tests/sdl-probe/sdl.c"],
        Linking::Dynamic,
        &args,
    )?;
    println!("built    {}", program.display());
    Ok(program)
}

/// Fails if `members` holds what only test images hold: the system tests,
/// selftest and its manifest.
fn check_no_tests(members: &[(&str, &Path)]) -> Result<()> {
    for (at, _) in members {
        if at.starts_with("system/bin/tests/")
            || *at == "system/bin/selftest"
            || *at == "system/programs/system.selftest.yaml"
        {
            bail!("{at} is in a system that is not a test image's");
        }
    }
    Ok(())
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
/// instrumentation, added to the kernel target's flags, and the
/// diagnostic build of the kernel its diag feature.
fn kernel_args(package: &str, kind: Kind) -> Vec<String> {
    if package != "kernel-x86_64" {
        return Vec::new();
    }
    if kind == Kind::Diag {
        return vec!["--features".into(), "diag".into()];
    }
    if kind != Kind::Fuzz {
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
        .args(kernel_args(package, kind))
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
    fn each_edition_has_its_boot_image() {
        assert_eq!(Edition::Server.boot_image(), BootImage::System);
        assert_eq!(Edition::Desktop.boot_image(), BootImage::Desktop);
    }

    #[test]
    fn a_system_that_is_not_a_test_image_holds_no_test() {
        let file = Path::new("file");
        let fine = [
            ("system/bin/msh", file),
            ("system/bin/tests", file),
            ("system/programs/system.msh.yaml", file),
        ];
        assert!(check_no_tests(&fine).is_ok());
        for at in [
            "system/bin/tests/webgpu",
            "system/bin/selftest",
            "system/programs/system.selftest.yaml",
        ] {
            let error = check_no_tests(&[("system/bin/msh", file), (at, file)]).unwrap_err();
            assert!(error.to_string().contains(at));
        }
    }

    #[test]
    fn instruments_only_the_fuzz_kernel() {
        assert!(kernel_args("kernel-x86_64", Kind::Debug).is_empty());
        assert!(kernel_args("mikrosboot", Kind::Fuzz).is_empty());
        let args = kernel_args("kernel-x86_64", Kind::Fuzz);
        assert_eq!(args[..3], ["--features", "fuzz", "--config"]);
        assert!(
            args[3].starts_with("target.x86_64-unknown-none.rustflags=[\"-Cpasses=sancov-module\"")
        );
    }

    #[test]
    fn gives_only_the_diag_kernel_its_feature() {
        assert_eq!(
            kernel_args("kernel-x86_64", Kind::Diag),
            ["--features", "diag"]
        );
        assert!(kernel_args("mikrosboot", Kind::Diag).is_empty());
        assert!(Kind::Production.user_rustflags().is_empty());
        assert!(
            Kind::Diag
                .user_rustflags()
                .contains(&"-Cforce-frame-pointers=yes")
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
