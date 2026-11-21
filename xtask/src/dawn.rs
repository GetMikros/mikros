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

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
pub struct Args {}

pub fn run(_args: &Args) -> Result<()> {
    let install = build()?;
    println!("built    {}", install.display());
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
