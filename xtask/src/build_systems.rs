//! Building C and C++ projects for Mikros with Meson and CMake, through
//! the sysroot's cross file and toolchain file, in
//! `target/ported/<name>/`. Projects are configured with the prefix
//! `/usr`, so the paths they build in are those of Mikros, and installed
//! below a directory of their own.
//!
//! A project is configured again, in an emptied directory, only when
//! its configuration changes: the command that configures it, the
//! contents of the cross or toolchain file, or its source. Otherwise the
//! build directory stays, and Ninja builds what changed in it.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn dir(name: &str) -> PathBuf {
    crate::workspace_root().join("target/ported").join(name)
}

/// The prefix projects are configured with.
const PREFIX: &str = "/usr";

/// Builds the CMake project in `source` as `name`, with the further
/// options `options`, and returns the directory its prefix was installed
/// in.
pub fn cmake(name: &str, source: &Path, options: &[String]) -> Result<PathBuf> {
    crate::sysroot::build()?;
    let cmake = crate::cmake::build()?;
    let toolchain = crate::sysroot::cmake_toolchain_file();
    let (build, staging) = (build_dir(name), dir(name).join("install"));
    let mut configure = Command::new(&cmake);
    configure
        .args(["-G", "Ninja", "-S"])
        .arg(source)
        .arg("-B")
        .arg(&build)
        .arg(format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain.display()))
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!("-DCMAKE_INSTALL_PREFIX={PREFIX}"))
        .args(options);
    configured(name, &mut configure, &toolchain, source)?;
    crate::port::run(Command::new(&cmake).arg("--build").arg(&build))
        .with_context(|| format!("building {name}"))?;
    crate::port::run(
        Command::new(&cmake)
            .arg("--install")
            .arg(&build)
            .env("DESTDIR", &staging)
            .stdout(Stdio::null()),
    )
    .with_context(|| format!("installing {name}"))?;
    Ok(staging.join(&PREFIX[1..]))
}

/// Builds the Meson project in `source` as `name`, with the further
/// options `options`, and returns the directory its prefix was installed
/// in.
pub fn meson(name: &str, source: &Path, options: &[String]) -> Result<PathBuf> {
    crate::sysroot::build()?;
    let cross = crate::sysroot::meson_cross_file();
    let (build, staging) = (build_dir(name), dir(name).join("install"));
    let mut setup = crate::meson::command()?;
    setup
        .arg("setup")
        .arg(format!("--cross-file={}", cross.display()))
        .args(["--buildtype=release", "--libdir=lib"])
        .arg(format!("--prefix={PREFIX}"))
        .args(options)
        .arg(&build)
        .arg(source);
    configured(name, &mut setup, &cross, source)?;
    crate::port::run(
        crate::meson::command()?
            .arg("compile")
            .arg("-C")
            .arg(&build),
    )
    .with_context(|| format!("building {name}"))?;
    crate::port::run(
        crate::meson::command()?
            .args(["install", "--quiet", "--only-changed", "-C"])
            .arg(&build)
            .env("DESTDIR", &staging)
            .stdout(Stdio::null()),
    )
    .with_context(|| format!("installing {name}"))?;
    Ok(staging.join(&PREFIX[1..]))
}

/// The build directory of the project built as `name`.
pub fn build_dir(name: &str) -> PathBuf {
    dir(name).join("build")
}

/// Configures the project built as `name` with `configure`, in an
/// emptied directory, unless it was configured by the same command, with
/// the same contents of the cross or toolchain file `file`, from the
/// same `source`. A source records how it was patched in a stamp file,
/// which changes whenever it is extracted again.
fn configured(name: &str, configure: &mut Command, file: &Path, source: &Path) -> Result<()> {
    let dir = dir(name);
    let stamp_path = dir.join(".mikros-configured");
    let stamp = format!(
        "{:?}\n{}\n{}\n",
        configure,
        fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?,
        fs::read_to_string(source.join(crate::port::STAMP)).unwrap_or_default(),
    );
    if fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(());
    }
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::port::run(configure).with_context(|| format!("configuring {name}"))?;
    fs::write(&stamp_path, stamp).with_context(|| format!("writing {}", stamp_path.display()))
}

/// Fails unless every one of `files`, relative to `install`, is there,
/// and those of `links` are symbolic links to their targets.
pub fn check_installed(install: &Path, files: &[&str], links: &[(&str, &str)]) -> Result<()> {
    for file in files {
        if !install.join(file).is_file() {
            bail!("{} has no {file}", install.display());
        }
    }
    for (link, target) in links {
        let found = fs::read_link(install.join(link)).ok();
        if found.as_deref() != Some(Path::new(target)) {
            bail!("{link} in {} is not a link to {target}", install.display());
        }
    }
    Ok(())
}
