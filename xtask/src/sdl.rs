//! SDL3, from the port in `user/ports/sdl3`: the static library
//! `libSDL3.slib`, position independent, with its headers and CMake
//! package. It is built from the fork's commit that the port pins,
//! against the libraries of the sysroot, which gets the libraries of the
//! ports that SDL links: libwayland, which `crate::wayland::build`
//! installs there, and xkbcommon, installed here.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// The library's file name in the install directory's `libraries`.
pub const LIBRARY: &str = "libSDL3.slib";

/// Writes `contents` to `path`, creating its directory, unless it holds
/// them already.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().context("a path without a directory")?)?;
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Installs the xkbcommon library, its headers and a pkg-config file that
/// links it in the sysroot, which is where the ports that link it find it.
fn install_xkbcommon() -> Result<()> {
    let port = crate::ports::load("xkbcommon")?;
    let built = crate::ports::build(&port)?;
    let sysroot = crate::sysroot::dir();
    let library = built.install.join("libraries/libxkbcommon.dlib");
    write_if_changed(
        &sysroot.join("lib/libxkbcommon.dlib"),
        &fs::read(&library).with_context(|| format!("reading {}", library.display()))?,
    )?;
    let headers = built.install.join("include/xkbcommon");
    for entry in fs::read_dir(&headers).with_context(|| format!("reading {}", headers.display()))? {
        let path = entry?.path();
        let name = path.file_name().context("a header without a name")?;
        write_if_changed(
            &sysroot.join("include/xkbcommon").join(name),
            &fs::read(&path)?,
        )?;
    }
    let pc = built.install.join("libraries/pkgconfig/xkbcommon.pc");
    let pc = fs::read_to_string(&pc).with_context(|| format!("reading {}", pc.display()))?;
    let pc = pc
        .replace("prefix=/system", "prefix=")
        .replace("${prefix}/libraries", "${prefix}/lib");
    write_if_changed(&sysroot.join("lib/pkgconfig/xkbcommon.pc"), pc.as_bytes())
}

/// The patched source of the port.
fn source(port: &crate::ports::Port) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/sources")
        .join(&port.name);
    let archive = dir.join(format!("{}.tar.gz", port.source));
    let what = format!("{} source", port.name);
    crate::fetch::cached(&what, &port.url, &port.sha256, &archive)?;
    crate::port::patched_source(&archive, &dir, &port.source, &port.dir.join("patches"), &[])
}

/// Builds SDL3, and returns the directory its prefix, `/system`, was
/// installed in.
pub fn build() -> Result<PathBuf> {
    let wayland = crate::wayland::build()?;
    install_xkbcommon()?;
    let port = crate::ports::load("sdl3")?;
    let source = source(&port)?;
    let sysroot = crate::sysroot::dir();
    let mut options = port.options.clone();
    options.push(format!(
        "-DWAYLAND_SCANNER={}",
        wayland.scanner_program().display()
    ));
    // pkg-config finds the sysroot's libraries alone, with their paths
    // below it.
    let libdir = sysroot.join("lib/pkgconfig");
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
