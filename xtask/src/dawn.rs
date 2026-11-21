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
//! With `--test` the command also builds a program that runs a compute
//! shader through Dawn's C API on the CPU Vulkan driver, `user/tests/webgpu`,
//! puts it in a system of its own as the only system test, and boots that
//! with selftest, as `cargo xtask test` does its system tests.

use crate::build::{self, Kind, Linking};
use crate::qemu::{MachineArgs, Processor};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The library's file name in the install directory's `libraries`.
pub const LIBRARY: &str = "libwebgpu_dawn.slib";

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
    /// Seconds the machine may go on without selftest writing a line.
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

/// Builds the test program of `install`'s Dawn, a C++ program that uses
/// Dawn's C API, linked dynamically with the C++ runtimes, and with Dawn
/// and the libraries of abseil that it needs.
fn build_program(install: &Path) -> Result<PathBuf> {
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
    build::c_build(
        "webgpu",
        &["user/tests/webgpu/webgpu.cpp"],
        Linking::Dynamic,
        &args,
    )
}

/// Builds the test program and the Dawn run's system, boots it, and fails
/// unless the test passes.
fn test(args: &Args, install: &Path) -> Result<()> {
    let program = build_program(install)?;
    println!("built    {}", program.display());
    let built = build::build_dawn_test(args.kind, &program)?;
    let system = built
        .dawn_image
        .as_deref()
        .context("the build made no system for the Dawn run")?;
    let dir = crate::workspace_root()
        .join("target/test")
        .join(args.kind.name())
        .join("dawn");
    let passed = crate::system_tests::run_with(
        &built,
        system,
        &args.machine,
        Processor::available(),
        args.kind,
        &dir,
        Duration::from_secs(args.timeout),
    )?;
    if passed != 1 {
        bail!("{passed} tests passed, one was expected");
    }
    println!("ok       the Dawn test passed");
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
