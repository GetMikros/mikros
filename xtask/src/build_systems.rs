//! Building C and C++ projects for Mikros with Meson and CMake, through
//! the sysroot's cross file and toolchain file, in
//! `target/ported/<name>/`. Projects are configured with the prefix
//! `/usr`, so the paths they build in are those of Mikros, and installed
//! below a directory of their own.

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
    let (build, staging) = fresh(name)?;
    crate::port::run(
        Command::new(&cmake)
            .args(["-G", "Ninja", "-S"])
            .arg(source)
            .arg("-B")
            .arg(&build)
            .arg(format!(
                "-DCMAKE_TOOLCHAIN_FILE={}",
                crate::sysroot::cmake_toolchain_file().display()
            ))
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .arg(format!("-DCMAKE_INSTALL_PREFIX={PREFIX}"))
            .args(options),
    )
    .with_context(|| format!("configuring {name}"))?;
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
    let (build, staging) = fresh(name)?;
    crate::port::run(
        crate::meson::command()?
            .arg("setup")
            .arg(format!(
                "--cross-file={}",
                crate::sysroot::meson_cross_file().display()
            ))
            .args(["--buildtype=release", "--libdir=lib"])
            .arg(format!("--prefix={PREFIX}"))
            .args(options)
            .arg(&build)
            .arg(source),
    )
    .with_context(|| format!("configuring {name}"))?;
    crate::port::run(
        crate::meson::command()?
            .arg("compile")
            .arg("-C")
            .arg(&build),
    )
    .with_context(|| format!("building {name}"))?;
    crate::port::run(
        crate::meson::command()?
            .args(["install", "--quiet", "-C"])
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

/// The build and the install directory of `name`, with what an earlier
/// build left removed: every build configures its project afresh, so
/// that changed tools and build system files take effect.
fn fresh(name: &str) -> Result<(PathBuf, PathBuf)> {
    let dir = dir(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok((build_dir(name), dir.join("install")))
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
