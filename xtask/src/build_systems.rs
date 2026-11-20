//! Building C and C++ projects for Mikros with Meson and CMake, through
//! the sysroot's cross file and toolchain file, in
//! `target/ported/<name>/`. Projects are configured with the prefix
//! `/system`, their libraries in `libraries/`, configuration in
//! `config/` and state in `state/`, so the paths they build in are those
//! of Mikros, and installed below a directory of their own.
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

/// The prefix projects are configured with, and their directories below
/// it.
const PREFIX: &str = "/system";
const LIBDIR: &str = "libraries";
const SYSCONFDIR: &str = "config";
const LOCALSTATEDIR: &str = "state";

/// Builds the CMake project in `source` as `name`, with the further
/// options `options`, and returns the directory its prefix was installed
/// in.
pub fn cmake(name: &str, source: &Path, options: &[String]) -> Result<PathBuf> {
    cmake_with_env(name, source, options, &[])
}

/// Builds the CMake project in `source` as `name`, as `cmake` does, with
/// the environment variables `env` set for configuring it, such as
/// those that point `pkg-config` at the sysroot.
pub fn cmake_with_env(
    name: &str,
    source: &Path,
    options: &[String],
    env: &[(&str, &Path)],
) -> Result<PathBuf> {
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
        .arg(format!("-DCMAKE_INSTALL_LIBDIR={LIBDIR}"))
        .arg(format!("-DCMAKE_INSTALL_SYSCONFDIR={SYSCONFDIR}"))
        .arg(format!("-DCMAKE_INSTALL_LOCALSTATEDIR={LOCALSTATEDIR}"))
        .args(options);
    set_env(&mut configure, env);
    configured(
        name,
        &mut configure,
        &toolchain,
        source,
        &crate::cmake::patches_stamp(),
    )?;
    // Ninja may configure the project again, with the same environment.
    let mut compile = Command::new(&cmake);
    compile.arg("--build").arg(&build);
    set_env(&mut compile, env);
    crate::port::run(&mut compile).with_context(|| format!("building {name}"))?;
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

/// Sets `env` on `command`, and with it, drops the build host's
/// `PKG_CONFIG_PATH`, which `pkg-config` would search before the
/// directories that `env` names.
fn set_env(command: &mut Command, env: &[(&str, &Path)]) {
    if !env.is_empty() {
        command
            .env_remove("PKG_CONFIG_PATH")
            .envs(env.iter().copied());
    }
}

/// Builds the Meson project in `source` as `name`, with the further
/// options `options`, and returns the directory its prefix was installed
/// in.
pub fn meson(name: &str, source: &Path, options: &[String]) -> Result<PathBuf> {
    crate::sysroot::build()?;
    // The CMake that the cross file names, for dependencies that Meson
    // finds through their CMake packages.
    crate::cmake::build()?;
    let cross = crate::sysroot::meson_cross_file();
    let (build, staging) = (build_dir(name), dir(name).join("install"));
    let mut setup = crate::meson::command()?;
    setup
        .arg("setup")
        .arg(format!("--cross-file={}", cross.display()))
        .arg("--buildtype=release")
        // A dependency that is missing fails the build instead of being
        // downloaded.
        .arg("--wrap-mode=nodownload")
        .arg(format!("--prefix={PREFIX}"))
        .arg(format!("--libdir={LIBDIR}"))
        .arg(format!("--sysconfdir={SYSCONFDIR}"))
        .arg(format!("--localstatedir={LOCALSTATEDIR}"))
        .args(options)
        .arg(&build)
        .arg(source);
    configured(
        name,
        &mut setup,
        &cross,
        source,
        &crate::meson::patches_stamp(),
    )?;
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
/// same `source`, by the build tool patched as `tool` records, against
/// the same C library's soname. A source
/// records how it was patched in a stamp file, which changes whenever it
/// is extracted again.
fn configured(
    name: &str,
    configure: &mut Command,
    file: &Path,
    source: &Path,
    tool: &str,
) -> Result<()> {
    let dir = dir(name);
    let stamp_path = dir.join(".mikros-configured");
    // The C library's soname, which the project's libraries and programs
    // name.
    let stamp = format!(
        "{:?}\n{}\n{}\n{tool}\n{}\n",
        configure,
        fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?,
        fs::read_to_string(source.join(crate::port::STAMP)).unwrap_or_default(),
        crate::musl::SONAME,
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

/// The `DT_SONAME` of the shared library at `path`, if it has one.
pub fn soname(path: &Path) -> Result<Option<String>> {
    use object::read::elf::{Dyn, FileHeader, SectionHeader};
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let header = object::elf::FileHeader64::<object::Endianness>::parse(&*data)
        .with_context(|| format!("{} is no ELF file", path.display()))?;
    let endian = header.endian()?;
    let sections = header.sections(endian, &*data)?;
    for section in sections.iter() {
        let Some((entries, link)) = section.dynamic(endian, &*data)? else {
            continue;
        };
        let strings = sections.strings(endian, &*data, link)?;
        for entry in entries {
            if entry.d_tag(endian) == object::elf::DT_SONAME {
                let name = strings
                    .get(entry.d_val(endian) as u32)
                    .map_err(|()| anyhow::anyhow!("{} has a bad soname", path.display()))?;
                return Ok(Some(String::from_utf8_lossy(name).into_owned()));
            }
        }
    }
    Ok(None)
}

/// Fails unless each shared library `libraries/<name>` in `install` has
/// its file name as its `DT_SONAME`, as a library of the system must.
pub fn check_sonames(install: &Path, names: &[&str]) -> Result<()> {
    for name in names {
        let path = install.join("libraries").join(name);
        let found = soname(&path)?;
        if found.as_deref() != Some(*name) {
            bail!("{} has the soname {found:?}, not {name}", path.display());
        }
    }
    Ok(())
}
