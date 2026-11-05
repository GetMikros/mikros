//! `cargo xtask sysroot`: the directory that C and C++ programs for
//! Mikros are built against, laid out like `/usr`.
//!
//! It holds the musl headers in `include/`, with the generated
//! `<mikros/syscalls.h>` and `<mikros/abi.h>` in `include/mikros/`;
//! `libc.slib`, `crti.o` and `crtn.o` in `lib/`, with
//! `lib/pkgconfig/`, and the Clang driver configuration file
//! `x86_64-unknown-mikros.cfg`, which selects the target, its CPU level
//! and this sysroot. The Clang that `crate::llvm` builds, which knows the
//! target, is used with it as
//! `clang --config=<sysroot>/x86_64-unknown-mikros.cfg`.

use crate::llvm::{self, LLVM};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Builds the toolchain and the sysroot, returning the sysroot's path.
pub fn build() -> Result<PathBuf> {
    llvm::build()?;
    check_llvm(&llvm::tool("clang"))?;
    check_llvm(&llvm::tool("ld.lld"))?;
    check_driver()?;
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
    let abi = crate::workspace_root().join("user/libc/include/mikros/abi.h");
    copy(&abi, &sysroot.join("include/mikros/abi.h"))?;
    let musl = crate::musl::build(&cfg, &sysroot)?;
    let lib = sysroot.join("lib");
    let layer = mikros_layer()?;
    combine(&lib.join("libc.slib"), &[&musl.libc, &layer])?;
    copy(&musl.crti, &lib.join("crti.o"))?;
    copy(&musl.crtn, &lib.join("crtn.o"))?;
    let crt1 = crate::workspace_root().join("user/libc/crt/crt1.S");
    crate::port::run(
        Command::new(llvm::tool("clang"))
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
    crate::port::run(
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
    let mut child = Command::new(llvm::tool("llvm-ar"))
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
fn check_llvm(tool: &Path) -> Result<()> {
    let shown = tool.display();
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .with_context(|| format!("running {shown} --version"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    match crate::tools::Version::find_in(&text) {
        Some(found) if found == LLVM => Ok(()),
        Some(found) => bail!("{shown} is LLVM {found}, but Mikros pins LLVM {LLVM}"),
        None => bail!("{shown} --version names no version"),
    }
}

/// Fails unless Clang compiles and links for the target with the
/// settings it must have: position-independent code with the strong
/// stack protector, linked by LLD as a position-independent executable
/// with the Mikros interpreter.
fn check_driver() -> Result<()> {
    let output = Command::new(llvm::tool("clang"))
        .args([
            &format!("--target={TRIPLE}"),
            "-###",
            "-x",
            "c",
            "-",
            "-o",
            "a.out",
        ])
        .output()
        .context("running clang -###")?;
    let text = String::from_utf8_lossy(&output.stderr);
    for wanted in [
        "\"-mrelocation-model\" \"pic\"",
        "\"-stack-protector\" \"2\"",
        "ld.lld\"",
        "\"-pie\"",
        "\"-dynamic-linker\" \"/usr/lib/ld-mikros.dlib\"",
        "crt1.o\"",
    ] {
        if !text.contains(wanted) {
            bail!("clang for {TRIPLE} lacks {wanted}:\n{text}");
        }
    }
    Ok(())
}

/// The Clang driver configuration of the target. `<CFGDIR>` is the
/// directory of the file, which is the sysroot. The rest of the target's
/// settings are the defaults of Clang's Mikros toolchain.
fn driver_config() -> String {
    [
        "# The Mikros target, written by cargo xtask sysroot.",
        &format!("--target={TRIPLE}"),
        "-march=x86-64-v3",
        "--sysroot=<CFGDIR>",
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
            "--sysroot=<CFGDIR>",
        ] {
            assert!(cfg.lines().any(|l| l == option), "{option}");
        }
    }
}
