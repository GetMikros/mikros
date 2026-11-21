//! SDL3, from the port in `user/ports/sdl3`: the static library
//! `libSDL3.slib`, position independent, with its headers and CMake
//! package. It is built from the fork's commit that the port pins,
//! against the libraries of the sysroot, which gets the libraries of the
//! ports that SDL links: libwayland, which `crate::wayland::build`
//! installs there, and xkbcommon, which `crate::system_libraries`
//! installs.

use anyhow::Result;
use std::path::PathBuf;

/// The library's file name in the install directory's `libraries`.
pub const LIBRARY: &str = "libSDL3.slib";

/// Builds SDL3, and returns the directory its prefix, `/system`, was
/// installed in.
pub fn build() -> Result<PathBuf> {
    let wayland = crate::wayland::build()?;
    crate::system_libraries::xkbcommon()?;
    let port = crate::ports::load("sdl3")?;
    let source = crate::ports::source(&port)?;
    let sysroot = crate::sysroot::dir();
    let mut options = port.options.clone();
    options.push(format!(
        "-DWAYLAND_SCANNER={}",
        wayland.scanner_program().display()
    ));
    // pkg-config finds the sysroot's libraries alone, with their paths
    // below it.
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
            "include/SDL3/SDL.h",
            "libraries/cmake/SDL3/SDL3Config.cmake",
        ],
        &[],
    )?;
    Ok(install)
}
