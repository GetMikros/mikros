//! The pinned musl release that the C library is a port of: fetching
//! it, applying the Mikros patches in `user/libc/patches/`, and building
//! its static library and headers with the sysroot's Clang
//! configuration.

use crate::port::{self, run};
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const VERSION: &str = "1.2.5";
const URL: &str = "https://musl.libc.org/releases/musl-1.2.5.tar.gz";
/// SHA-256 of the release archive at `URL`.
const SHA256: &str = "a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4";
/// The file in the patched source that records which patches it has.
const STAMP: &str = ".mikros-patches";

fn dir() -> PathBuf {
    crate::workspace_root().join("target/musl")
}

fn patches_dir() -> PathBuf {
    crate::workspace_root().join("user/libc/patches")
}

/// What building musl produced.
pub struct Built {
    /// musl's static library, before the Mikros layer is added.
    pub libc: PathBuf,
    /// The objects that a program is linked with around its own code.
    pub crti: PathBuf,
    pub crtn: PathBuf,
}

/// Fetches, patches and builds musl with the Clang configuration `cfg`,
/// and installs its headers into `sysroot`.
pub fn build(cfg: &Path, sysroot: &Path) -> Result<Built> {
    let archive = dir().join(format!("musl-{VERSION}.tar.gz"));
    crate::fetch::cached(&format!("musl {VERSION}"), URL, SHA256, &archive)?;
    let source = patched_source(&archive)?;
    let build = dir().join("build");
    let clang = crate::llvm::tool("clang");
    let cc = format!("{} --config={}", clang.display(), cfg.display());
    // A build configured with another compiler is configured again.
    let configured = fs::read_to_string(build.join("config.mak")).unwrap_or_default();
    if !configured.lines().any(|l| l == format!("CC = {cc}")) {
        fs::create_dir_all(&build)?;
        run(Command::new(source.join("configure"))
            .current_dir(&build)
            .args([
                "--target=x86_64",
                "--prefix=/",
                "--includedir=/include",
                "--libdir=/lib",
                "--disable-shared",
            ])
            .env("CC", &cc)
            .env("AR", crate::llvm::tool("llvm-ar"))
            .env("RANLIB", crate::llvm::tool("llvm-ranlib")))
        .context("configuring musl")?;
    }
    let jobs = format!(
        "-j{}",
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    run(Command::new("make").current_dir(&build).args([
        "-s",
        &jobs,
        "lib/libc.a",
        "lib/crti.o",
        "lib/crtn.o",
    ]))
    .context("building musl")?;
    run(Command::new("make")
        .current_dir(&build)
        .args(["-s", "install-headers"])
        .arg(format!("DESTDIR={}", sysroot.display())))
    .context("installing the musl headers")?;
    Ok(Built {
        libc: build.join("lib/libc.a"),
        crti: build.join("lib/crti.o"),
        crtn: build.join("lib/crtn.o"),
    })
}

/// The musl source with every patch applied, extracted again from
/// `archive` whenever the patches changed. The build directory is
/// removed with it, so that nothing built from older sources stays.
fn patched_source(archive: &Path) -> Result<PathBuf> {
    let patches = port::patches(&patches_dir())?;
    let stamp = port::stamp(&patches)?;
    let source = dir().join(format!("musl-{VERSION}"));
    if fs::read_to_string(source.join(STAMP)).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(source);
    }
    for stale in [&source, &dir().join("build")] {
        if stale.exists() {
            fs::remove_dir_all(stale).with_context(|| format!("removing {}", stale.display()))?;
        }
    }
    let gz = flate2::read::GzDecoder::new(File::open(archive)?);
    tar::Archive::new(gz)
        .unpack(dir())
        .with_context(|| format!("extracting {}", archive.display()))?;
    for patch in &patches {
        run(Command::new("patch")
            .args(["-p1", "-s", "-d"])
            .arg(&source)
            .arg("-i")
            .arg(patch))
        .with_context(|| format!("applying {}", patch.display()))?;
    }
    fs::write(source.join(STAMP), stamp)?;
    Ok(source)
}
