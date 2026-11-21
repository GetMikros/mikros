//! The system libraries in the sysroot (MIKSPEC-0062.R13): what
//! applications may link besides the C and C++ libraries, which
//! `crate::sysroot` builds, with their headers and pkg-config files, and
//! the protocol files of the Wayland extensions of Mikros. libwayland,
//! with its cursor and EGL window libraries, and the protocol files of
//! wayland-protocols come from `crate::wayland`; xkbcommon and the Vulkan
//! headers and loader, with `vulkan_mikros.h`, from their ports.
//! Libraries lie in `libraries/`, their pkg-config files in
//! `libraries/pkgconfig/` with an empty prefix, and the Mikros protocol
//! files in `share/wayland-protocols/mikros/`.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Writes `contents` to `path`, creating its directory, unless it holds
/// them already.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|old| old == contents) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().context("a path without a directory")?)?;
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Copies the files below `from` to `to`, keeping their layout.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let entries = fs::read_dir(from).with_context(|| format!("reading {}", from.display()))?;
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().context("a file without a name")?;
        match path.is_dir() {
            true => copy_tree(&path, &to.join(name))?,
            false => write_if_changed(&to.join(name), &fs::read(&path)?)?,
        }
    }
    Ok(())
}

/// The pkg-config file `pc` of a port's install, whose prefix is
/// `/system`, for the sysroot, whose prefix is empty.
fn sysroot_pc(pc: &Path) -> Result<String> {
    let text = fs::read_to_string(pc).with_context(|| format!("reading {}", pc.display()))?;
    Ok(text.replace("prefix=/system", "prefix="))
}

/// Builds every system library and installs it in the sysroot, which
/// `crate::sysroot::build` made.
pub fn install() -> Result<()> {
    crate::wayland::build()?;
    crate::wayland::install_protocols()?;
    mikros_protocols()?;
    xkbcommon()?;
    vulkan()
}

/// Installs the protocol files of the Mikros Wayland extensions, which
/// the compositor's package holds.
fn mikros_protocols() -> Result<()> {
    let from = crate::workspace_root().join("user/bin/compositor/protocols");
    let to = crate::sysroot::dir().join("share/wayland-protocols/mikros");
    let entries = fs::read_dir(&from).with_context(|| format!("reading {}", from.display()))?;
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().context("a file without a name")?;
        if name.to_string_lossy().starts_with("mikros-") && path.extension() == Some("xml".as_ref())
        {
            write_if_changed(&to.join(name), &fs::read(&path)?)?;
        }
    }
    Ok(())
}

/// Installs the xkbcommon library, its headers and its pkg-config file.
pub fn xkbcommon() -> Result<()> {
    let port = crate::ports::load("xkbcommon")?;
    let built = crate::ports::build(&port)?;
    let sysroot = crate::sysroot::dir();
    let library = built.install.join("libraries/libxkbcommon.dlib");
    write_if_changed(
        &sysroot.join("libraries/libxkbcommon.dlib"),
        &fs::read(&library).with_context(|| format!("reading {}", library.display()))?,
    )?;
    copy_tree(
        &built.install.join("include/xkbcommon"),
        &sysroot.join("include/xkbcommon"),
    )?;
    let pc = sysroot_pc(&built.install.join("libraries/pkgconfig/xkbcommon.pc"))?;
    write_if_changed(
        &sysroot.join("libraries/pkgconfig/xkbcommon.pc"),
        pc.as_bytes(),
    )
}

/// Installs the Vulkan headers, with those of the Mikros extensions, the
/// loader and its pkg-config file.
fn vulkan() -> Result<()> {
    let sysroot = crate::sysroot::dir();
    let headers = crate::vulkan::headers()?.join("include");
    for dir in ["vulkan", "vk_video"] {
        copy_tree(&headers.join(dir), &sysroot.join("include").join(dir))?;
    }
    let loader = crate::vulkan::loader()?.join("libraries");
    let library = loader.join(crate::vulkan::LOADER);
    write_if_changed(
        &sysroot.join("libraries").join(crate::vulkan::LOADER),
        &fs::read(&library).with_context(|| format!("reading {}", library.display()))?,
    )?;
    let pc = sysroot_pc(&loader.join("pkgconfig/vulkan.pc"))?;
    write_if_changed(
        &sysroot.join("libraries/pkgconfig/vulkan.pc"),
        pc.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_pkg_config_files_to_the_sysroot() {
        let dir = std::env::temp_dir().join(format!("system-libraries-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let pc = dir.join("vulkan.pc");
        let text = "prefix=/system\nexec_prefix=${prefix}\nlibdir=${exec_prefix}/libraries\n\
                    includedir=${prefix}/include\nLibs: -L${libdir} -lvulkan\n";
        fs::write(&pc, text).unwrap();
        let moved = sysroot_pc(&pc).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(moved.starts_with("prefix=\n"), "{moved}");
        assert!(
            moved.contains("libdir=${exec_prefix}/libraries\n"),
            "{moved}"
        );
        assert!(moved.contains("includedir=${prefix}/include\n"), "{moved}");
    }
}
