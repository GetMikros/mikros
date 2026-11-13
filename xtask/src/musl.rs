//! The musl release that the C library is a port of, which
//! `user/ports/musl` pins: fetching it, applying the Mikros patches in
//! `user/ports/musl/patches/`, and building
//! its static library and headers with the sysroot's Clang
//! configuration.

use crate::port::{self, run};
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn dir() -> PathBuf {
    crate::workspace_root().join("target/musl")
}

/// The `DT_SONAME` of the shared C library.
pub const SONAME: &str = "libc.1.dlib";

/// What building musl produced.
pub struct Built {
    /// musl's static library, before the Mikros layer is added.
    pub libc: PathBuf,
    /// The shared library, with the Mikros layer, which is also the
    /// dynamic linker.
    pub shared: PathBuf,
    /// The objects that a program is linked with around its own code.
    pub crti: PathBuf,
    pub crtn: PathBuf,
}

/// Fetches, patches and configures musl with the Clang configuration
/// `cfg`, and installs its headers into `sysroot`. Neither needs the
/// compiler-rt builtins, which are built against these headers.
pub fn install_headers(cfg: &Path, sysroot: &Path) -> Result<()> {
    let build = configured(cfg)?;
    run(Command::new("make")
        .current_dir(&build)
        .args(["-s", "install-headers"])
        .arg(format!("DESTDIR={}", sysroot.display())))
    .context("installing the musl headers")
}

/// Builds musl, configured by `install_headers` with the Clang
/// configuration `cfg`, linking `extra`, the object of the shared
/// variant of the Mikros layer and the ABI note, into the shared
/// library.
pub fn build(cfg: &Path, extra: &[&Path]) -> Result<Built> {
    let build = configured(cfg)?;
    // make does not see the objects it links into the shared library
    // besides musl's own, so the library is linked again when one of them
    // is newer than it.
    let shared = build.join("lib/libc.so");
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    if let Some(linked) = modified(&shared) {
        let builtins = crate::llvm::builtins();
        let linked_in = extra.iter().copied().chain([builtins.as_path()]);
        if linked_in
            .into_iter()
            .any(|p| modified(p).is_none_or(|m| m > linked))
        {
            fs::remove_file(&shared)?;
        }
    }
    let jobs = format!(
        "-j{}",
        std::thread::available_parallelism().map_or(1, |n| n.get())
    );
    run(Command::new("make").current_dir(&build).args([
        "-s",
        &jobs,
        "lib/libc.a",
        "lib/libc.so",
        "lib/crti.o",
        "lib/crtn.o",
        &format!(
            "LIBCC={} {}",
            extra
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" "),
            crate::llvm::builtins().display()
        ),
    ]))
    .context("building musl")?;
    Ok(Built {
        libc: build.join("lib/libc.a"),
        shared,
        crti: build.join("lib/crti.o"),
        crtn: build.join("lib/crtn.o"),
    })
}

/// The build directory of musl, fetched, patched and configured with
/// the Clang configuration `cfg`.
fn configured(cfg: &Path) -> Result<PathBuf> {
    let port = crate::ports::load("musl")?;
    let archive = dir().join(format!("{}.tar.gz", port.source));
    crate::fetch::cached(&port.source, &port.url, &port.sha256, &archive)?;
    let source = patched_source(&archive, &port)?;
    let build = dir().join("build");
    let clang = crate::llvm::tool("clang");
    let cc = format!("{} --config={}", clang.display(), cfg.display());
    let ldflags = format!("-Wl,-soname,{SONAME}");
    let options = [
        "--target=x86_64",
        "--prefix=/",
        "--includedir=/include",
        "--libdir=/lib",
        "--enable-shared",
    ];
    // A build configured otherwise is configured again.
    let stamp = format!("{cc}\n{ldflags}\n{}", options.join("\n"));
    let configured = build.join(".mikros-configure");
    if fs::read_to_string(&configured).ok() != Some(stamp.clone()) {
        fs::create_dir_all(&build)?;
        run(Command::new(source.join("configure"))
            .current_dir(&build)
            .args(options)
            .env("CC", &cc)
            .env("LDFLAGS", &ldflags)
            .env("AR", crate::llvm::tool("llvm-ar"))
            .env("RANLIB", crate::llvm::tool("llvm-ranlib")))
        .context("configuring musl")?;
        fs::write(&configured, stamp)?;
    }
    Ok(build)
}

/// The musl source with every patch applied. The build directory is
/// removed whenever the source is extracted again, so that nothing built
/// from older sources stays.
fn patched_source(archive: &Path, port: &crate::ports::Port) -> Result<PathBuf> {
    port::patched_source(
        archive,
        &dir(),
        &port.source,
        &port.dir.join("patches"),
        &[&dir().join("build")],
    )
}
