//! The Wayland client library, `libwayland.dlib`, from the port in
//! `user/ports/wayland`: the library of `wayland-scanner` and the static
//! client, cursor and EGL window libraries built for Mikros, linked into
//! one shared library whose `DT_SONAME` is its file name in
//! `/system/libraries`.
//!
//! The scanner that generates its protocol code runs on the build host
//! and is built there first from the same patched source, since the
//! build for Mikros needs a scanner of its own version.
//!
//! The library, its headers and a pkg-config file that links it are
//! installed in the sysroot, as are the protocol files of
//! `user/ports/wayland-protocols` with theirs, for the ported projects
//! that use them. The protocol files that the compositor serves, the core
//! protocol's of this port and some of wayland-protocols, are staged in
//! `target/wayland-protocols` for its build script.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The library's file name in `/system/libraries`, which is its soname.
pub const LIBRARY: &str = "libwayland.dlib";
/// The static client library of the build for Mikros, in its build
/// directory, which also holds the objects of the static libraries it
/// uses, and which the shared library holds.
const ARCHIVE: &str = "src/libwayland-client.slib";
/// The static cursor and EGL window libraries, in the same build
/// directory.
const OTHER_ARCHIVES: [&str; 2] = ["cursor/libwayland-cursor.slib", "egl/libwayland-egl.slib"];

/// What the build gives.
pub struct Built {
    /// The shared library.
    pub library: PathBuf,
    /// The directory its prefix, `/system`, was installed in, with the
    /// headers.
    pub install: PathBuf,
    /// The directory of the pkg-config file of the build host's
    /// `wayland-scanner`.
    pub scanner: PathBuf,
}

impl Built {
    /// The build host's `wayland-scanner`, in the prefix of its pkg-config
    /// file's directory.
    pub fn scanner_program(&self) -> PathBuf {
        let prefix = self.scanner.ancestors().nth(2).unwrap_or(&self.scanner);
        prefix.join("bin/wayland-scanner")
    }
}

/// Writes `contents` to `path`, creating its directory, unless it holds
/// them already, so that what is built from it is not built again.
fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().context("a path without a directory")?)?;
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Installs the client, cursor and EGL window libraries of `built` and
/// their headers in the sysroot, with pkg-config files that link
/// `libwayland.dlib`.
fn install_client(built: &Built) -> Result<()> {
    let sysroot = crate::sysroot::dir();
    let include = built.install.join("include");
    let entries =
        fs::read_dir(&include).with_context(|| format!("reading {}", include.display()))?;
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().context("a header without a name")?;
        write_if_changed(&sysroot.join("include").join(name), &fs::read(&path)?)?;
    }
    write_if_changed(
        &sysroot.join("libraries").join(LIBRARY),
        &fs::read(&built.library)?,
    )?;
    for name in ["wayland-client", "wayland-cursor", "wayland-egl"] {
        let flag = format!("-l{name}");
        let pc = built.install.join(format!("libraries/pkgconfig/{name}.pc"));
        let pc = fs::read_to_string(&pc).with_context(|| format!("reading {}", pc.display()))?;
        let pc = pc
            .replace("prefix=/system", "prefix=")
            .replace(&flag, "-lwayland");
        write_if_changed(
            &sysroot.join(format!("libraries/pkgconfig/{name}.pc")),
            pc.as_bytes(),
        )?;
    }
    Ok(())
}

/// Installs the protocol files of the wayland-protocols port in the
/// sysroot's `share/wayland-protocols`, laid out as in its source, with
/// its pkg-config file.
pub fn install_protocols() -> Result<()> {
    let port = crate::ports::load("wayland-protocols")?;
    let source = source(&port)?;
    let share = crate::sysroot::dir().join("share");
    let mut dirs: Vec<PathBuf> = ["stable", "staging", "unstable", "experimental"]
        .iter()
        .map(|d| source.join(d))
        .collect();
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "xml") {
                let relative = path.strip_prefix(&source)?;
                let to = share.join("wayland-protocols").join(relative);
                write_if_changed(&to, &fs::read(&path)?)?;
            }
        }
    }
    let version = port
        .source
        .strip_prefix("wayland-protocols-")
        .context("the source directory names no version")?;
    let pc = format!(
        "pkgdatadir=${{pc_sysrootdir}}/share/wayland-protocols\n\n\
         Name: Wayland Protocols\nDescription: Wayland protocol files\nVersion: {version}\n"
    );
    write_if_changed(&share.join("pkgconfig/wayland-protocols.pc"), pc.as_bytes())
}

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

/// The files of the wayland-protocols port that the compositor serves,
/// relative to its source.
const PROTOCOLS: [&str; 5] = [
    "stable/xdg-shell/xdg-shell.xml",
    "stable/viewporter/viewporter.xml",
    "staging/fractional-scale/fractional-scale-v1.xml",
    "staging/xdg-activation/xdg-activation-v1.xml",
    "staging/cursor-shape/cursor-shape-v1.xml",
];

/// Puts the protocol files that the compositor serves in
/// `target/wayland-protocols`, where its build script reads them, leaving
/// no others there, and returns that directory. A file is written only
/// when its contents changed.
pub fn protocols() -> Result<PathBuf> {
    let core = source(&crate::ports::load("wayland")?)?.join("protocol/wayland.xml");
    let others = source(&crate::ports::load("wayland-protocols")?)?;
    let files: Vec<PathBuf> = std::iter::once(core)
        .chain(PROTOCOLS.iter().map(|p| others.join(p)))
        .collect();
    let dir = crate::workspace_root().join("target/wayland-protocols");
    fs::create_dir_all(&dir)?;
    let names: Vec<_> = files.iter().filter_map(|f| f.file_name()).collect();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !names.contains(&entry.file_name().as_os_str()) {
            fs::remove_file(entry.path())?;
        }
    }
    for file in &files {
        let text = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        let staged = dir.join(file.file_name().context("a protocol file without a name")?);
        if fs::read(&staged).ok().as_deref() != Some(text.as_slice()) {
            fs::write(&staged, &text)?;
        }
    }
    Ok(dir)
}

/// Builds `wayland-scanner` for the build host from `source`, and returns
/// the directory of its pkg-config file.
fn host_scanner(source: &Path) -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/ported/wayland-scanner");
    let (build, install) = (dir.join("build"), dir.join("install"));
    if !build.join("build.ninja").exists() {
        fs::create_dir_all(&dir)?;
        crate::port::run(
            crate::meson::command()?
                .arg("setup")
                .arg("--buildtype=release")
                .arg(format!("--prefix={}", install.display()))
                .arg("--libdir=lib")
                .args([
                    "-Dlibraries=false",
                    "-Dscanner=true",
                    "-Dtests=false",
                    "-Ddocumentation=false",
                    "-Ddtd_validation=false",
                ])
                .arg(&build)
                .arg(source),
        )
        .context("configuring wayland-scanner for the build host")?;
    }
    crate::port::run(
        crate::meson::command()?
            .args(["install", "--quiet", "--only-changed", "-C"])
            .arg(&build)
            .stdout(std::process::Stdio::null()),
    )
    .context("building wayland-scanner for the build host")?;
    Ok(install.join("lib/pkgconfig"))
}

/// Builds `libwayland.dlib`.
pub fn build() -> Result<Built> {
    let port = crate::ports::load("wayland")?;
    let source = source(&port)?;
    let scanner = host_scanner(&source)?;
    let mut options = port.options.clone();
    options.push(format!("-Dbuild.pkg_config_path={}", scanner.display()));
    let install = crate::build_systems::meson(&port.name, &source, &options)?;
    let build = crate::build_systems::build_dir(&port.name);
    let library = build.join(LIBRARY);
    link(&build, &library)?;
    let built = Built {
        library,
        install,
        scanner,
    };
    install_client(&built)?;
    Ok(built)
}

/// Links the static client library of the build in `build` into the
/// shared library `library`, all of it, exported as its visibility says.
fn link(build: &Path, library: &Path) -> Result<()> {
    let cfg = crate::sysroot::dir().join("x86_64-unknown-mikros.cfg");
    crate::port::run(
        Command::new(crate::llvm::tool("clang"))
            .arg(format!("--config={}", cfg.display()))
            .arg("-shared")
            .arg(format!("-Wl,-soname,{LIBRARY}"))
            .arg("-Wl,--whole-archive")
            .arg(build.join(ARCHIVE))
            .args(OTHER_ARCHIVES.iter().map(|a| build.join(a)))
            .arg("-Wl,--no-whole-archive")
            .arg("-o")
            .arg(library),
    )
    .with_context(|| format!("linking {LIBRARY}"))
}
