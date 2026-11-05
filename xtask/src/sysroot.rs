//! `cargo xtask sysroot`: the directory that C and C++ programs for
//! Mikros are built against, laid out like `/usr`.
//!
//! It holds the musl headers in `include/`, with the generated
//! `<mikros/syscalls.h>` in `include/mikros/`;
//! `libc.slib`, `crti.o` and `crtn.o` in `lib/`, with
//! `lib/pkgconfig/`, and the Clang driver configuration file
//! `x86_64-unknown-mikros.cfg`, which selects the target, its options
//! and this sysroot. A compiler is used with it as
//! `clang --config=<sysroot>/x86_64-unknown-mikros.cfg`.

use crate::tools::Version;
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The pinned LLVM release whose Clang and LLD build C and C++ code.
pub const LLVM: Version = Version(23, 1, 4);
/// The target triple of C and C++ code.
pub const TRIPLE: &str = "x86_64-unknown-mikros";

/// The directories of the sysroot, relative to it.
const DIRS: [&str; 4] = ["include", "include/mikros", "lib", "lib/pkgconfig"];

#[derive(clap::Args)]
pub struct Args {}

pub fn run(_args: &Args) -> Result<()> {
    let sysroot = build()?;
    println!("built    {}", sysroot.display());
    Ok(())
}

/// Where the sysroot is built.
pub fn dir() -> PathBuf {
    crate::workspace_root().join("target/sysroot")
}

/// Checks the toolchain and builds the sysroot, returning its path.
pub fn build() -> Result<PathBuf> {
    check_llvm("clang")?;
    check_llvm("ld.lld")?;
    let sysroot = dir();
    for d in DIRS {
        let path = sysroot.join(d);
        fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
    }
    let cfg = config(&sysroot);
    fs::write(&cfg, driver_config()).with_context(|| format!("writing {}", cfg.display()))?;
    let header = sysroot.join("include/mikros/syscalls.h");
    fs::write(&header, syscalls_header()?)
        .with_context(|| format!("writing {}", header.display()))?;
    let musl = crate::musl::build(&cfg, &sysroot)?;
    let lib = sysroot.join("lib");
    let layer = mikros_layer()?;
    combine(&lib.join("libc.slib"), &[&musl.libc, &layer])?;
    copy(&musl.crti, &lib.join("crti.o"))?;
    copy(&musl.crtn, &lib.join("crtn.o"))?;
    let crt1 = crate::workspace_root().join("user/libc/crt/crt1.S");
    crate::musl::run(
        Command::new("clang")
            .arg(format!("--config={}", cfg.display()))
            .arg("-c")
            .arg(&crt1)
            .arg("-o")
            .arg(lib.join("crt1.o")),
    )
    .context("assembling crt1.o")?;
    Ok(sysroot)
}

/// Builds the Mikros layer of the C library, a static library of the
/// `user` workspace, and returns its path.
fn mikros_layer() -> Result<PathBuf> {
    let root = crate::workspace_root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    crate::musl::run(
        Command::new(cargo)
            .current_dir(root.join("user"))
            .args([
                "build",
                "--release",
                "--package",
                "mikros-libc",
                "--lib",
                "--target",
            ])
            .arg(root.join(crate::build::USER_TARGET)),
    )
    .context("building the Mikros layer of the C library")?;
    Ok(root.join("user/target/x86_64-unknown-mikros/release/libmikros_libc.slib"))
}

/// Writes the static library `to` with the members of every archive of
/// `from`.
fn combine(to: &Path, from: &[&Path]) -> Result<()> {
    let mut script = format!("create {}\n", to.display());
    for archive in from {
        script += &format!("addlib {}\n", archive.display());
    }
    script += "save\nend\n";
    let mut child = Command::new("llvm-ar")
        .arg("-M")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("running llvm-ar")?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(script.as_bytes())?;
    if !child.wait()?.success() {
        bail!("combining the archives of {} failed", to.display());
    }
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .map(drop)
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))
}

/// `<mikros/syscalls.h>`, generated from the system call table.
fn syscalls_header() -> Result<String> {
    let path = crate::workspace_root().join("kernel/syscalls.yaml");
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let read = syscall_table::table::read(&bytes)
        .map_err(|e| anyhow::anyhow!("{}:{e}", path.display()))?;
    Ok(syscall_table::c::header(&read.table, "x86_64"))
}

/// The Clang driver configuration file of `sysroot`.
pub fn config(sysroot: &Path) -> PathBuf {
    sysroot.join(format!("{TRIPLE}.cfg"))
}

/// Fails unless `tool --version` reports the pinned LLVM release.
fn check_llvm(tool: &str) -> Result<()> {
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .with_context(|| format!("running {tool} --version; LLVM {LLVM} is needed"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    match Version::find_in(&text) {
        Some(found) if found == LLVM => Ok(()),
        Some(found) => bail!("{tool} is LLVM {found}, but Mikros pins LLVM {LLVM}"),
        None => bail!("{tool} --version names no version; LLVM {LLVM} is needed"),
    }
}

/// The Clang driver configuration of the target. `<CFGDIR>` is the
/// directory of the file, which is the sysroot.
fn driver_config() -> String {
    [
        "# The Mikros target, written by cargo xtask sysroot.",
        &format!("--target={TRIPLE}"),
        "-march=x86-64-v3",
        "-fPIC",
        "-fstack-protector-strong",
        "--sysroot=<CFGDIR>",
        "-isystem",
        "<CFGDIR>/include",
        "-fuse-ld=lld",
        "-pie",
        "-Wl,--dynamic-linker=/usr/lib/ld-mikros.dlib",
        "",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_selects_the_target_and_its_options() {
        let cfg = driver_config();
        for option in [
            "--target=x86_64-unknown-mikros",
            "-march=x86-64-v3",
            "-fPIC",
            "-fstack-protector-strong",
            "-fuse-ld=lld",
            "-pie",
            "-Wl,--dynamic-linker=/usr/lib/ld-mikros.dlib",
        ] {
            assert!(cfg.lines().any(|l| l == option), "{option}");
        }
    }
}
