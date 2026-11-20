//! The Khronos Vulkan headers and loader, from the ports in
//! `user/ports/vulkan-headers` and `user/ports/vulkan-loader`. The loader
//! is `libvulkan.dlib`, through which programs reach the drivers that
//! their manifests in `/system/share/vulkan/icd.d/` name.

use anyhow::Result;
use std::path::PathBuf;

/// The loader's file name in `/system/libraries`, which is its soname.
pub const LOADER: &str = "libvulkan.dlib";

/// The CMake options of the loader: the Wayland surface functions without
/// those of X11, and the directories that it searches for manifests and
/// settings, which are `/system/share` and `/system/config`, never
/// `/etc`.
const LOADER_OPTIONS: [&str; 8] = [
    "-DBUILD_WSI_XCB_SUPPORT=OFF",
    "-DBUILD_WSI_XLIB_SUPPORT=OFF",
    "-DBUILD_WSI_WAYLAND_SUPPORT=ON",
    "-DBUILD_WSI_DIRECTFB_SUPPORT=OFF",
    "-DBUILD_TESTS=OFF",
    "-DFALLBACK_DATA_DIRS=/system/share",
    "-DFALLBACK_CONFIG_DIRS=/system/config",
    "-DSYSCONFDIR=/system/config",
];

/// The patched source of the port `port`.
fn source(port: &crate::ports::Port) -> Result<PathBuf> {
    let dir = crate::workspace_root()
        .join("target/sources")
        .join(&port.name);
    let archive = dir.join(format!("{}.tar.gz", port.source));
    let what = format!("{} source", port.name);
    crate::fetch::cached(&what, &port.url, &port.sha256, &archive)?;
    crate::port::patched_source(&archive, &dir, &port.source, &port.dir.join("patches"), &[])
}

/// Installs the headers, and returns the directory their prefix,
/// `/system`, was installed in.
pub fn headers() -> Result<PathBuf> {
    let port = crate::ports::load("vulkan-headers")?;
    let source = source(&port)?;
    let install = crate::build_systems::cmake(&port.name, &source, &[])?;
    crate::build_systems::check_installed(&install, &["include/vulkan/vulkan.h"], &[])?;
    Ok(install)
}

/// Builds the loader, and returns the directory its prefix, `/system`,
/// was installed in.
pub fn loader() -> Result<PathBuf> {
    let headers = headers()?;
    let port = crate::ports::load("vulkan-loader")?;
    let source = source(&port)?;
    let mut options: Vec<String> = LOADER_OPTIONS.iter().map(|o| o.to_string()).collect();
    options.push(format!(
        "-DVULKAN_HEADERS_INSTALL_DIR={}",
        headers.display()
    ));
    options.push(format!("-DCMAKE_PREFIX_PATH={}", headers.display()));
    let install = crate::build_systems::cmake(&port.name, &source, &options)?;
    crate::build_systems::check_installed(&install, &[&format!("libraries/{LOADER}")], &[])?;
    crate::build_systems::check_sonames(&install, &[LOADER])?;
    Ok(install)
}
