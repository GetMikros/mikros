//! uutils coreutils, the command-line utilities of Mikros, built with
//! the standard library of Mikros: the pinned release, the pinned crates
//! of its dependencies that need Mikros changes, and those changes. The
//! rewrite rules `REWRITES` widen Linux with musl to Mikros in every file
//! of the release and of the crates in `REWRITTEN`, when they are
//! unpacked; the other changes are the patch files in `ports/coreutils/`,
//! `patches/` for the release and `crates/<name>-<version>/` for a
//! crate. Each utility of `UTILITIES` is its own program.

use anyhow::{Context, Result, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const URL: &str = "https://github.com/uutils/coreutils/archive/refs/tags/0.12.0.tar.gz";
const SHA256: &str = "4fb327655cb4ffcbf2f16550cf9234079ffe839692f7aa1a6eda104af684e122";
const SOURCE: &str = "coreutils-0.12.0";

/// The crates that need Mikros changes, by name and version, with the
/// checksums that the release's `Cargo.lock` gives them, and whether the
/// rewrite rules apply to them.
const CRATES: [(&str, &str, &str, bool); 10] = [
    (
        "libc",
        "0.2.189",
        "3eaf3ede3fee6db1a4c2ee091bf8a8b4dccdc6d17f656fb07896ee72867612f2",
        true,
    ),
    (
        "nix",
        "0.31.3",
        "cf20d2fde8ff38632c426f1165ed7436270b44f199fc55284c38276f9db47c3d",
        true,
    ),
    (
        "rustix",
        "1.1.4",
        "b6fe4565b9518b83ef4f91bb47ce29620ca828bd32cb7e408f0062e9930ba190",
        true,
    ),
    (
        "errno",
        "0.3.14",
        "39cab71617ae0d63f51a36d69f866391735b51691dbda63cf6f96d042b63efeb",
        true,
    ),
    (
        "procfs",
        "0.18.0",
        "25485360a54d6861439d60facef26de713b1e126bf015ec8f98239467a2b82f7",
        true,
    ),
    (
        "procfs-core",
        "0.18.0",
        "e6401bf7b6af22f78b563665d15a22e9aef27775b79b149a66ca022468a4e405",
        true,
    ),
    (
        "socket2",
        "0.6.5",
        "c3d1e2c7f27f8d4cb10542a02c49005dbd6e93095799d6f3be745fae9f8fedd4",
        true,
    ),
    (
        "mio",
        "1.2.2",
        "30d65c71f1ce40ab09135ce117d742b9f8a19ff91a41a8b57ed50bc2de59c427",
        true,
    ),
    // Rewritten, they would make their own Linux system calls; their
    // patches have them use the C library's getrandom instead.
    (
        "getrandom",
        "0.4.3",
        "300e883d756b2e4ec94e02791f39b04b522276138852cfc41d9fb7e904106099",
        false,
    ),
    (
        "getrandom",
        "0.2.17",
        "ff2abc00be7fca6ebc474524697ae276ad847ad0a6b3faa4bcb027e9a4614ad0",
        false,
    ),
];

/// The rewrite rules: Mikros where Linux, or Linux with musl, is meant.
const REWRITES: [(&str, &str); 2] = [
    (
        r#"target_os = "linux""#,
        r#"any(target_os = "linux", target_os = "mikros")"#,
    ),
    (
        r#"target_env = "musl""#,
        r#"any(target_env = "musl", target_os = "mikros")"#,
    ),
];

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

fn dir() -> PathBuf {
    crate::workspace_root().join("target/ported/coreutils")
}

/// Applies `REWRITES` to every file under `dir` that is UTF-8 text.
fn rewrite(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            rewrite(&path)?;
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut changed = text.clone();
        for (from, to) in REWRITES {
            changed = changed.replace(from, to);
        }
        if changed != text {
            fs::write(&path, changed).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

/// A digest of the rewrite rules, whether they apply, and `patches`.
fn stamp(rewritten: bool, patches: &[PathBuf]) -> Result<String> {
    let rules: String = REWRITES
        .iter()
        .map(|(a, b)| format!("{a}\n{b}\n"))
        .collect();
    Ok(format!(
        "{}\n{}{}",
        crate::port::stamp(patches)?,
        rewritten,
        if rewritten { rules } else { String::new() }
    ))
}

/// The source `into/<source>`, unpacked from the gzip-compressed tar
/// `archive`, rewritten when `rewritten`, with the patches in `patches`
/// applied. It is unpacked again whenever the changes differ.
fn unpack(
    archive: &Path,
    into: &Path,
    source: &str,
    rewritten: bool,
    patches: &Path,
) -> Result<PathBuf> {
    let patches = crate::port::patches(patches)?;
    let stamp = stamp(rewritten, &patches)?;
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
        rewrite(&source)?;
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
    let root = crate::workspace_root();
    let ports = root.join("ports/coreutils");
    let archive = dir().join(format!("{SOURCE}.tar.gz"));
    crate::fetch::cached("coreutils source", URL, SHA256, &archive)?;
    let crates = dir().join("crates");
    for (name, version, sha256, rewritten) in CRATES {
        let full = format!("{name}-{version}");
        let archive = dir().join(format!("{full}.crate"));
        let url = format!("https://static.crates.io/crates/{name}/{full}.crate");
        crate::fetch::cached(&format!("{full} crate"), &url, sha256, &archive)?;
        unpack(
            &archive,
            &crates,
            &full,
            rewritten,
            &ports.join("crates").join(&full),
        )?;
    }
    // The release's patch makes Cargo take those crates from `crates`.
    let source = unpack(&archive, &dir(), SOURCE, true, &ports.join("patches"))?;
    let packages: Vec<String> = UTILITIES.iter().map(|u| format!("uu_{u}")).collect();
    let mut cargo = crate::rust_std::cargo(&source, &RUSTFLAGS)?;
    cargo
        .env("RUSTUP_TOOLCHAIN", toolchain()?)
        .env("CARGO_TARGET_DIR", dir().join("target"))
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
