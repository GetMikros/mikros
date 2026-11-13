//! uutils coreutils, the command-line utilities of Mikros, built with
//! the standard library of Mikros: the release, the crates of its
//! dependencies that need Mikros changes, and those changes, which
//! `user/ports/coreutils` pins and holds. The port's rewrites apply to
//! every file of the release and of the crates that say they are
//! rewritten, when they are unpacked; the other changes are the patch
//! files, `patches/` for the release and `crates/<name>-<version>/` for a
//! crate. Each utility of `UTILITIES` is its own program.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// The flags of the build besides the standard ones: rustix uses the C
/// library, not its own Linux system calls, and the warnings of the
/// release and its crates, which are not Mikros' code, are not shown.
const RUSTFLAGS: [&str; 2] = ["--cfg=rustix_use_libc", "--cap-lints=allow"];

/// The utilities that the system installs, by their upstream names: all
/// of the release's but `stdbuf`, whose preloaded library Mikros cannot
/// load, `chcon` and `runcon`, which need SELinux, `mkfifo` and `mknod`,
/// since its file systems have no FIFOs or device files, and `chroot`,
/// since it has namespaces instead.
#[rustfmt::skip]
pub const UTILITIES: [&str; 102] = [
    "arch", "b2sum", "base32", "base64", "basename", "basenc", "cat", "chgrp", "chmod", "chown",
    "cksum", "comm", "cp", "csplit", "cut", "date", "dd", "df", "dir", "dircolors", "dirname", "du",
    "echo", "env", "expand", "expr", "factor", "false", "fmt", "fold", "groups", "head", "hostid",
    "hostname", "id", "install", "join", "kill", "link", "ln", "logname", "ls", "md5sum", "mkdir",
    "mktemp", "more", "mv", "nice", "nl", "nohup", "nproc", "numfmt", "od", "paste", "pathchk",
    "pinky", "pr", "printenv", "printf", "ptx", "pwd", "readlink", "realpath", "rm", "rmdir", "seq",
    "sha1sum", "sha224sum", "sha256sum", "sha384sum", "sha512sum", "shred", "shuf", "sleep", "sort",
    "split", "stat", "stty", "sum", "sync", "tac", "tail", "tee", "test", "timeout", "touch", "tr",
    "true", "truncate", "tsort", "tty", "uname", "unexpand", "uniq", "unlink", "uptime", "users",
    "vdir", "wc", "who", "whoami", "yes",
];

/// The file in an unpacked source that records the changes it has.
const STAMP: &str = ".mikros-changes";

/// Cargo's target directory for the port.
pub fn target_dir() -> PathBuf {
    dir().join("target")
}

fn dir() -> PathBuf {
    crate::workspace_root().join("target/ported/coreutils")
}

/// Applies `rules` to every file under `dir` that is UTF-8 text.
fn rewrite(dir: &Path, rules: &[(String, String)]) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            rewrite(&path, rules)?;
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut changed = text.clone();
        for (from, to) in rules {
            changed = changed.replace(from.as_str(), to);
        }
        if changed != text {
            fs::write(&path, changed).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

/// A digest of the rewrite rules `rules`, whether they apply, and
/// `patches`.
fn stamp(rules: &[(String, String)], rewritten: bool, patches: &[PathBuf]) -> Result<String> {
    let rules: String = rules.iter().map(|(a, b)| format!("{a}\n{b}\n")).collect();
    Ok(format!(
        "{}\n{}{}",
        crate::port::stamp(patches)?,
        rewritten,
        if rewritten { rules } else { String::new() }
    ))
}

/// The source `into/<source>`, unpacked from the gzip-compressed tar
/// `archive`, rewritten with `rules` when `rewritten`, with the patches in
/// `patches` applied. It is unpacked again whenever the changes differ.
fn unpack(
    archive: &Path,
    into: &Path,
    source: &str,
    (rules, rewritten): (&[(String, String)], bool),
    patches: &Path,
) -> Result<PathBuf> {
    let patches = crate::port::patches(patches)?;
    let stamp = stamp(rules, rewritten, &patches)?;
    let source = into.join(source);
    if fs::read_to_string(source.join(STAMP)).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(source);
    }
    if source.exists() {
        fs::remove_dir_all(&source).with_context(|| format!("removing {}", source.display()))?;
    }
    fs::create_dir_all(into)?;
    println!("extracting {}", archive.display());
    let gz = flate2::read::GzDecoder::new(File::open(archive)?);
    tar::Archive::new(gz)
        .unpack(into)
        .with_context(|| format!("extracting {}", archive.display()))?;
    if rewritten {
        rewrite(&source, rules)?;
    }
    for patch in &patches {
        crate::port::run(
            std::process::Command::new("patch")
                .args(["-p1", "-s", "-d"])
                .arg(&source)
                .arg("-i")
                .arg(patch),
        )
        .with_context(|| format!("applying {}", patch.display()))?;
    }
    fs::write(source.join(STAMP), stamp)?;
    Ok(source)
}

/// The toolchain that the workspace's `rust-toolchain.toml` names, which
/// builds the release too.
fn toolchain() -> Result<String> {
    let file = crate::workspace_root().join("rust-toolchain.toml");
    let text = fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .map(|c| c.trim_matches('"').to_owned())
        .with_context(|| format!("{} names no channel", file.display()))
}

/// Fetches, changes and builds the utilities, and gives their programs
/// by name. They are always built optimized, which makes them much
/// smaller.
pub fn build() -> Result<Vec<(String, PathBuf)>> {
    let port = crate::ports::load("coreutils")?;
    let rules = port.rewrites.as_slice();
    let archive = dir().join(format!("{}.tar.gz", port.source));
    crate::fetch::cached("coreutils source", &port.url, &port.sha256, &archive)?;
    let crates = dir().join("crates");
    for c in &port.crates {
        let full = format!("{}-{}", c.name, c.version);
        let archive = dir().join(format!("{full}.crate"));
        let url = format!("https://static.crates.io/crates/{}/{full}.crate", c.name);
        crate::fetch::cached(&format!("{full} crate"), &url, &c.sha256, &archive)?;
        unpack(
            &archive,
            &crates,
            &full,
            (rules, c.rewritten),
            &port.dir.join("crates").join(&full),
        )?;
    }
    // The release's patch makes Cargo take those crates from `crates`.
    let source = unpack(
        &archive,
        &dir(),
        &port.source,
        (rules, true),
        &port.dir.join("patches"),
    )?;
    let packages: Vec<String> = UTILITIES.iter().map(|u| format!("uu_{u}")).collect();
    let mut cargo = crate::rust_std::cargo(&source, &RUSTFLAGS)?;
    cargo
        .env("RUSTUP_TOOLCHAIN", toolchain()?)
        .env("CARGO_TARGET_DIR", target_dir())
        .arg("--release");
    for package in &packages {
        cargo.args(["--package", package]);
    }
    let status = cargo.status().context("failed to run cargo")?;
    if !status.success() {
        bail!("building coreutils failed");
    }
    let out = dir().join("target/x86_64-unknown-mikros/release");
    UTILITIES
        .iter()
        .map(|u| {
            let program = out.join(u);
            match program.exists() {
                true => Ok((u.to_string(), program)),
                false => bail!("coreutils built no {}", program.display()),
            }
        })
        .collect()
}
