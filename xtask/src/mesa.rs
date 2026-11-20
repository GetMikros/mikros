//! Mesa for Mikros, from the port in `user/ports/mesa`: lavapipe, the CPU
//! Vulkan driver, as `libvulkan_lvp.dlib` with its driver manifest,
//! built against `libLLVM.dlib` of the llvm port (`crate::llvm_port`),
//! which Meson finds through LLVM's CMake package, and with the shaders
//! that Mesa builds into it compiled by glslang on the build host
//! (`crate::glslang`). Its Wayland surfaces use `libwayland.dlib` and the
//! protocol files that `crate::wayland` installs in the sysroot, and the
//! build host's `wayland-scanner`.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// The driver's file name in `/system/libraries`, which is its soname.
pub const DRIVER: &str = "libvulkan_lvp.dlib";
/// Its driver manifest, relative to the prefix, in the directory of
/// Vulkan driver manifests.
pub const MANIFEST: &str = "share/vulkan/icd.d/lvp_icd.x86_64.json";

/// The patched source of the port.
fn source(port: &crate::ports::Port) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/sources")
        .join(&port.name);
    let archive = dir.join(format!("{}.tar.xz", port.source));
    let what = format!("{} source", port.name);
    crate::fetch::cached(&what, &port.url, &port.sha256, &archive)?;
    crate::port::patched_source(&archive, &dir, &port.source, &port.dir.join("patches"), &[])
}

/// The header of the Mikros extensions, which the Mesa port adds to
/// Mesa's copy of the Vulkan headers and the vulkan-headers port to the
/// headers that programs are built against.
const EXTENSIONS: &str = "include/vulkan/vulkan_mikros.h";

/// Fails unless Mesa, in `source`, defines the Mikros extensions as the
/// headers of programs do.
fn check_extensions(source: &Path) -> Result<()> {
    let programs = crate::vulkan::headers()?.join(EXTENSIONS);
    let read = |path: &Path| fs::read(path).with_context(|| format!("reading {}", path.display()));
    if read(&source.join(EXTENSIONS))? != read(&programs)? {
        bail!("the {EXTENSIONS} of the mesa and vulkan-headers ports differ");
    }
    Ok(())
}

/// Builds Mesa, and returns the directory its prefix, `/system`, was
/// installed in.
pub fn build() -> Result<PathBuf> {
    let port = crate::ports::load("mesa")?;
    let source = source(&port)?;
    check_extensions(&source)?;
    let llvm = crate::llvm_port::build_library()?;
    let wayland = crate::wayland::build()?;
    crate::wayland::install_protocols()?;
    let mut options = port.options.clone();
    options.push(format!(
        "-Dbuild.pkg_config_path={}",
        wayland.scanner.display()
    ));
    options.push(format!("-Dcmake_prefix_path={}", llvm.display()));
    options.push(format!(
        "--native-file={}",
        crate::glslang::native_file()?.display()
    ));
    let install = crate::build_systems::meson(&port.name, &source, &options)?;
    crate::build_systems::check_installed(
        &install,
        &[&format!("libraries/{DRIVER}"), MANIFEST],
        &[],
    )?;
    crate::build_systems::check_sonames(&install, &[DRIVER])?;
    Ok(install)
}
