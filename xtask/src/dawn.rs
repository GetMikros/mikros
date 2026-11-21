//! Dawn, from the port in `user/ports/dawn`: the static library
//! `libwebgpu_dawn.slib` that holds Dawn and its shader compiler, Tint,
//! for the Vulkan and Wayland platforms of Mikros, with its headers. It is
//! built from the fork's commit that the port pins, against the sysroot.
//!
//! Dawn builds its dependencies with its own CMake files, from the
//! directories that this module gives it: the sources of the ports
//! abseil, spirv-headers, spirv-tools and vulkan-utility-libraries, and
//! the patched source of the Vulkan headers port, which has the Mikros
//! extensions. The build takes a long time, which is why it is a command
//! of its own and not a part of `cargo xtask build`.
//!
//! With `--test` the command also builds two programs and boots two
//! systems. `user/tests/webgpu` runs a compute shader through Dawn's C API
//! on the CPU Vulkan driver; it is the only system test of a system of its
//! own, which is booted with selftest, as `cargo xtask test` does its
//! system tests. `user/tests/dawn-sdl` renders a frame with Dawn to a
//! window of SDL3's Wayland driver; it is the switcher part of the shell
//! probe in a desktop system, which is booted through the desktop run's
//! scenario, whose check is that the window shows the frame's colour.

use crate::build::{self, DawnTest, Kind, Linking};
use crate::desktop::Window;
use crate::qemu::{MachineArgs, Processor};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The library's file name in the install directory's `libraries`.
pub const LIBRARY: &str = "libwebgpu_dawn.slib";

/// The window of the Dawn window program: the colour of the triangle that
/// covers its frame, and its size.
const WINDOW: Window = Window {
    colour: [30, 190, 90],
    size: (128, 96),
    what: "the Dawn window showed",
};

/// The memory of the machine of the desktop run unless `--memory` says
/// otherwise, as in `cargo xtask test`.
const DESKTOP_MEMORY: &str = "3G";

/// The ports that Dawn builds with its own files, and the option that
/// names the directory of the source of each.
const DEPENDENCIES: [(&str, &str); 4] = [
    ("abseil", "DAWN_ABSEIL_DIR"),
    ("spirv-headers", "DAWN_SPIRV_HEADERS_DIR"),
    ("spirv-tools", "DAWN_SPIRV_TOOLS_DIR"),
    (
        "vulkan-utility-libraries",
        "DAWN_VULKAN_UTILITY_LIBRARIES_DIR",
    ),
];

#[derive(clap::Args)]
pub struct Args {
    /// Also build the test program, boot a system whose only test it is,
    /// and fail unless it passes.
    #[arg(long)]
    test: bool,
    /// The build kind of the system to test.
    #[arg(long, value_enum, default_value_t = Kind::Debug, requires = "test")]
    kind: Kind,
    /// Seconds the compute test's machine may go on without selftest writing
    /// a line, and the whole desktop run's scenario may take.
    #[arg(long, default_value_t = 240, requires = "test")]
    timeout: u64,
    #[command(flatten)]
    machine: MachineArgs,
}

pub fn run(args: &Args) -> Result<()> {
    let install = build()?;
    println!("built    {}", install.display());
    if args.test {
        test(args, &install)?;
    }
    Ok(())
}

/// The arguments that link a program with Dawn, the static library of
/// `install`, and the libraries of abseil that it needs, and that find
/// Dawn's headers.
fn link_args(install: &Path) -> Result<Vec<String>> {
    let libraries = install.join("libraries");
    let mut args = vec![
        "-Wall".to_string(),
        "-Wextra".to_string(),
        format!("-I{}", install.join("include").display()),
        "-Wl,--start-group".to_string(),
        libraries.join(LIBRARY).display().to_string(),
    ];
    let mut abseil = Vec::new();
    for entry in fs::read_dir(&libraries)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with("libabsl_") && name.ends_with(".slib") {
            abseil.push(libraries.join(name).display().to_string());
        }
    }
    abseil.sort();
    args.extend(abseil);
    args.push("-Wl,--end-group".to_string());
    Ok(args)
}

/// Builds the test program of `install`'s Dawn, a C++ program that uses
/// Dawn's C API, linked dynamically with the C++ runtimes, and with Dawn
/// and the libraries of abseil that it needs.
fn build_program(install: &Path) -> Result<PathBuf> {
    build::c_build(
        "webgpu",
        &["user/tests/webgpu/webgpu.cpp"],
        Linking::Dynamic,
        &link_args(install)?,
    )
}

/// Builds the window program of `install`'s Dawn, which draws to a window of
/// SDL3's: linked as the test program is, with SDL3, statically, and the
/// libraries that it needs, Wayland, xkbcommon and the Vulkan loader,
/// dynamically.
fn build_window_program(install: &Path) -> Result<PathBuf> {
    let sdl = crate::sdl::build()?;
    let wayland = crate::wayland::build()?;
    let xkbcommon = crate::ports::build(&crate::ports::load("xkbcommon")?)?;
    let vulkan = crate::vulkan::loader()?;
    let mut args = link_args(install)?;
    args.extend([
        "-Werror".to_string(),
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
    ]);
    build::c_build(
        "dawn-sdl",
        &["user/tests/dawn-sdl/dawn_sdl.cpp"],
        Linking::Dynamic,
        &args,
    )
}

/// Builds the programs and the Dawn run's systems, boots the system of the
/// compute test, and fails unless its test passes, and boots the desktop
/// system, and fails unless the window shows the frame.
fn test(args: &Args, install: &Path) -> Result<()> {
    let compute = build_program(install)?;
    println!("built    {}", compute.display());
    let window = build_window_program(install)?;
    println!("built    {}", window.display());
    let built = build::build_dawn_test(
        args.kind,
        &DawnTest {
            compute: &compute,
            window: &window,
        },
    )?;
    let system = built
        .dawn_image
        .as_deref()
        .context("the build made no system for the Dawn run")?;
    let dir = crate::workspace_root()
        .join("target/test")
        .join(args.kind.name())
        .join("dawn");
    let timeout = Duration::from_secs(args.timeout);
    let passed = crate::system_tests::run_with(
        &built,
        system,
        &args.machine,
        Processor::available(),
        args.kind,
        &dir,
        timeout,
    )?;
    if passed != 1 {
        bail!("{passed} tests passed, one was expected");
    }
    println!("ok       the Dawn test passed");
    let system = built
        .dawn_desktop_image
        .as_deref()
        .context("the build made no desktop system for the Dawn run")?;
    let mut machine = args.machine.clone();
    machine
        .memory
        .get_or_insert_with(|| DESKTOP_MEMORY.to_owned());
    crate::desktop::run_with(
        &machine,
        args.kind,
        &dir.join("desktop"),
        timeout,
        &built,
        system,
        &WINDOW,
    )?;
    println!("ok       the Dawn window showed its frame");
    Ok(())
}

/// Fails unless the host's Python has the packages that Dawn's source
/// generators import.
fn check_python() -> Result<()> {
    let found = Command::new("python3")
        .args(["-c", "import jinja2, markupsafe"])
        .stderr(Stdio::null())
        .status()
        .context("running python3")?;
    if !found.success() {
        bail!(
            "Dawn's source generators need the Python packages jinja2 and markupsafe (apt: python3-jinja2)"
        );
    }
    Ok(())
}

/// Builds Dawn, and returns the directory its prefix, `/system`, was
/// installed in.
pub fn build() -> Result<PathBuf> {
    check_python()?;
    // The sysroot gets the Wayland client library and headers that Dawn's
    // Wayland surface uses.
    crate::wayland::build()?;
    let port = crate::ports::load("dawn")?;
    let source = crate::ports::source(&port)?;
    let mut options = port.options.clone();
    for (name, option) in DEPENDENCIES {
        let dependency = crate::ports::load(name)?;
        let dir = crate::ports::source(&dependency)?;
        options.push(format!("-D{option}={}", dir.display()));
    }
    let headers = crate::vulkan::headers_source()?;
    options.push(format!("-DDAWN_VULKAN_HEADERS_DIR={}", headers.display()));
    let sysroot = crate::sysroot::dir();
    let libdir = sysroot.join("libraries/pkgconfig");
    let install = crate::build_systems::cmake_with_env(
        &port.name,
        &source,
        &options,
        &[
            ("PKG_CONFIG_LIBDIR", libdir.as_path()),
            ("PKG_CONFIG_SYSROOT_DIR", sysroot.as_path()),
        ],
    )?;
    crate::build_systems::check_installed(
        &install,
        &[
            &format!("libraries/{LIBRARY}"),
            // The libraries of abseil that the library needs to be linked
            // with, which Dawn's build installs with it.
            "libraries/libabsl_base.slib",
            "include/dawn/webgpu.h",
            "include/dawn/webgpu_cpp.h",
            "include/dawn/native/DawnNative.h",
        ],
        &[],
    )?;
    Ok(install)
}
