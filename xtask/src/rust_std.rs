//! The Rust standard library for Mikros: the toolchain's `library/`
//! source with the Mikros patches in `user/runtime/std/patches/`, and the
//! programs of the `user/runtime/std` workspace built against it.
//!
//! Cargo builds the standard library from source (`-Zbuild-std`) out
//! of the copy in `target/rust-std/library`. In that copy, the vendored
//! `libc` crate takes `mikros` for Linux with musl, whose C library and
//! headers Mikros has, and the workspace takes `libc` from there.

use crate::build::Kind;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The file in the copy that records the toolchain and patches it was
/// made from.
const STAMP: &str = ".mikros-patches";

fn dir() -> PathBuf {
    crate::workspace_root().join("target/rust-std")
}

/// The toolchain's standard library source.
fn toolchain_library() -> Result<PathBuf> {
    let output = Command::new("rustc")
        .current_dir(crate::workspace_root())
        .args(["--print", "sysroot"])
        .output()
        .context("running rustc")?;
    if !output.status.success() {
        bail!("rustc --print sysroot failed");
    }
    let sysroot = String::from_utf8(output.stdout)?;
    let library = Path::new(sysroot.trim()).join("lib/rustlib/src/rust/library");
    if !library.is_dir() {
        bail!("{} is missing: install rust-src", library.display());
    }
    Ok(library)
}

/// The patched copy of the standard library source, made again from
/// the toolchain's whenever the toolchain or the patches changed. Cargo
/// does not see changes in it, so the target directories of everything
/// built with it, `user/runtime/std` and coreutils, are removed then as
/// well.
pub fn source() -> Result<PathBuf> {
    let root = crate::workspace_root();
    let toolchain = toolchain_library()?;
    let patches = crate::port::patches(&root.join("user/runtime/std/patches"))?;
    let stamp = format!(
        "{}\n{}\n",
        toolchain.display(),
        crate::port::stamp(&patches)?
    );
    let library = dir().join("library");
    if fs::read_to_string(dir().join(STAMP)).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(library);
    }
    for stale in [
        dir(),
        root.join("user/runtime/std/target"),
        crate::coreutils::target_dir(),
    ] {
        if stale.exists() {
            fs::remove_dir_all(&stale).with_context(|| format!("removing {}", stale.display()))?;
        }
    }
    fs::create_dir_all(dir())?;
    println!("copying  {}", toolchain.display());
    crate::port::run(Command::new("cp").arg("-r").arg(&toolchain).arg(&library))
        .context("copying the standard library source")?;
    let libc = vendored_libc(&library)?;
    port_libc(&libc)?;
    let manifest = library.join("Cargo.toml");
    let text = fs::read_to_string(&manifest)?;
    let anchor = "windows-sys = { path = 'windows-sys' }\n";
    if !text.contains(anchor) {
        bail!(
            "{} has no windows-sys patch to add libc after",
            manifest.display()
        );
    }
    let libc_path = libc.strip_prefix(&library)?.display().to_string();
    let text = text.replacen(
        anchor,
        &format!("{anchor}libc = {{ path = '{libc_path}' }}\n"),
        1,
    );
    fs::write(&manifest, text)?;
    for patch in &patches {
        crate::port::run(
            Command::new("patch")
                .args(["-p1", "-s", "-d"])
                .arg(&library)
                .arg("-i")
                .arg(patch),
        )
        .with_context(|| format!("applying {}", patch.display()))?;
    }
    fs::write(dir().join(STAMP), stamp)?;
    Ok(library)
}

/// The `libc` crate in `library/vendor`.
fn vendored_libc(library: &Path) -> Result<PathBuf> {
    let vendor = library.join("vendor");
    let mut found = Vec::new();
    for entry in fs::read_dir(&vendor).with_context(|| format!("reading {}", vendor.display()))? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("libc-0."))
        {
            found.push(path);
        }
    }
    match found.as_slice() {
        [one] => Ok(one.clone()),
        _ => bail!("expected one libc crate in {}", vendor.display()),
    }
}

/// The replacements that make the `libc` crate's Rust code take
/// `mikros` for Linux with musl.
const LIBC_CFGS: [(&str, &str); 2] = [
    (
        r#"target_os = "linux""#,
        r#"any(target_os = "linux", target_os = "mikros")"#,
    ),
    (
        r#"target_env = "musl""#,
        r#"any(target_env = "musl", target_os = "mikros")"#,
    ),
];

/// The replacements in the `libc` crate's build script, each of which
/// must apply: its Linux settings hold for `mikros` too.
const LIBC_BUILD: [(&str, &str); 2] = [
    (
        r#"if target_os == "linux" && target_ptr_width == "64" {"#,
        r#"if matches!(target_os.as_str(), "linux" | "mikros") && target_ptr_width == "64" {"#,
    ),
    (
        r#"assert_eq!(target_os, "linux", "{cfg:?} set on non-linux platform");"#,
        r#"assert!(matches!(target_os, "linux" | "mikros"), "{cfg:?} set on non-linux platform");"#,
    ),
];

/// Ports the `libc` crate at `crate_dir` to `mikros`.
fn port_libc(crate_dir: &Path) -> Result<()> {
    for file in rust_files(&crate_dir.join("src"))? {
        let text = fs::read_to_string(&file)?;
        let ported = rewrite(&text, &LIBC_CFGS);
        if ported != text {
            fs::write(&file, ported)?;
        }
    }
    let build = crate_dir.join("build.rs");
    let mut text = fs::read_to_string(&build)?;
    for (from, to) in LIBC_BUILD {
        if !text.contains(from) {
            bail!("{} has no `{from}`", build.display());
        }
        text = text.replace(from, to);
    }
    fs::write(&build, text)?;
    Ok(())
}

fn rewrite(text: &str, replacements: &[(&str, &str)]) -> String {
    let mut text = text.to_owned();
    for (from, to) in replacements {
        text = text.replace(from, to);
    }
    text
}

/// The `.rs` files under `dir`.
fn rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(rust_files(&path)?);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    Ok(files)
}

/// `cargo build` in the workspace `dir` for the Mikros target, with the
/// standard library built from the patched source, linked by the
/// sysroot's Clang with the C library and libunwind, and with
/// `rustflags` besides.
pub fn cargo(dir: &Path, rustflags: &[&str]) -> Result<Command> {
    let root = crate::workspace_root();
    let sysroot = crate::sysroot::build()?;
    let library = source()?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let linker = crate::llvm::tool("clang");
    let config = crate::sysroot::config(&sysroot);
    let mut flags = vec![
        format!("-Clinker={}", linker.display()),
        "-Clinker-flavor=gcc".to_owned(),
        format!("-Clink-arg=--config={}", config.display()),
        // Clang links every Mikros executable as position-independent,
        // and so has no use for the `-pie` that rustc passes.
        "-Clink-arg=-Wno-unused-command-line-argument".to_owned(),
    ];
    flags.extend(rustflags.iter().map(|f| f.to_string()));
    let mut command = Command::new(cargo);
    command
        .current_dir(dir)
        .env("__CARGO_TESTS_ONLY_SRC_ROOT", &library)
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
        .args([
            "-Zjson-target-spec",
            "-Zbuild-std=std,panic_unwind",
            "-Zbuild-std-features=panic-unwind",
        ])
        .args(["build", "--target"])
        .arg(root.join(crate::build::USER_TARGET));
    Ok(command)
}

/// The programs that `prebuild` built, by build kind and name.
static PREBUILT: Mutex<BTreeSet<(&str, String)>> = Mutex::new(BTreeSet::new());

/// Builds the programs `bins` of the packages of the same names in the
/// `user/runtime/std` workspace in one run of Cargo. `build` then finds
/// them built.
pub fn prebuild(bins: &[String], kind: Kind) -> Result<()> {
    let root = crate::workspace_root();
    let mut command = cargo(&root.join("user/runtime/std"), kind.user_rustflags())?;
    for bin in bins {
        command.args(["--package", bin]);
    }
    let status = command
        .args(["--bins", "--profile", kind.profile()])
        .status()
        .context("failed to run cargo")?;
    if !status.success() {
        bail!("building the programs with the standard library failed");
    }
    let mut prebuilt = PREBUILT.lock().expect("not poisoned");
    prebuilt.extend(bins.iter().map(|b| (kind.name(), b.clone())));
    Ok(())
}

/// Builds the program `bin` of the package of the same name in the
/// `user/runtime/std` workspace, which uses the standard library, unless
/// `prebuild` did, and returns its path. It is linked by the sysroot's
/// Clang, with the C library and libunwind.
pub fn build(bin: &str, kind: Kind) -> Result<PathBuf> {
    let root = crate::workspace_root();
    let path = root
        .join("user/runtime/std/target/x86_64-unknown-mikros")
        .join(kind.profile_dir())
        .join(bin);
    if PREBUILT
        .lock()
        .expect("not poisoned")
        .contains(&(kind.name(), bin.to_owned()))
    {
        return Ok(path);
    }
    let status = cargo(&root.join("user/runtime/std"), kind.user_rustflags())?
        .args(["--package", bin, "--bin", bin])
        .args(["--profile", kind.profile()])
        .status()
        .context("failed to run cargo")?;
    if !status.success() {
        bail!("building {bin} failed");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_the_linux_cfgs() {
        let text = r#"#[cfg(all(target_os = "linux", not(target_env = "musl")))]"#;
        assert_eq!(
            rewrite(text, &LIBC_CFGS),
            r#"#[cfg(all(any(target_os = "linux", target_os = "mikros"), not(any(target_env = "musl", target_os = "mikros"))))]"#
        );
    }
}
