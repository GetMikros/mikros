//! The pinned CMake release that builds ported C and C++ projects for
//! Mikros with the sysroot's toolchain file: fetching its source,
//! applying the Mikros patches in `tools/cmake/patches/`, and building
//! and installing it in `target/cmake/install` with the host's CMake.

use crate::tools::Version;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The pinned CMake release.
pub const CMAKE: Version = Version(4, 4, 5);
const URL: &str = "https://github.com/Kitware/CMake/releases/download/v4.4.5/cmake-4.4.5.tar.gz";
/// SHA-256 of the source archive at `URL`.
const SHA256: &str = "a0c54c88abb8078dd8eccadb717fc6bb194bf18e7d44c86309910288886e04e2";
const SOURCE: &str = "cmake-4.4.5";
/// The options of the build: CMake itself, without its tests, dialogs
/// or TLS support.
const OPTIONS: [&str; 7] = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_C_COMPILER=clang",
    "-DCMAKE_CXX_COMPILER=clang++",
    "-DBUILD_TESTING=OFF",
    "-DBUILD_CursesDialog=OFF",
    "-DBUILD_QtDialog=OFF",
    "-DCMAKE_USE_OPENSSL=OFF",
];

fn dir() -> PathBuf {
    crate::workspace_root().join("target/cmake")
}

/// Fetches, patches, builds and installs CMake, and returns the path of
/// the installed `cmake`. A build that is up to date only checks the
/// patches and runs Ninja.
pub fn build() -> Result<PathBuf> {
    let archive = dir().join(format!("{SOURCE}.tar.gz"));
    crate::fetch::cached(&format!("CMake {CMAKE} source"), URL, SHA256, &archive)?;
    let build = dir().join("build");
    let source = crate::port::patched_source(
        &archive,
        &dir(),
        SOURCE,
        &crate::workspace_root().join("tools/cmake/patches"),
        &[&build],
    )?;
    let install = dir().join("install");
    let mut args = vec![format!("-DCMAKE_INSTALL_PREFIX={}", install.display())];
    args.extend(OPTIONS.iter().map(|o| o.to_string()));
    // A build configured with other options is configured again.
    let configured = build.join(".mikros-cmake-options");
    if fs::read_to_string(&configured).ok() != Some(args.join("\n")) {
        fs::create_dir_all(&build)?;
        crate::port::run(
            Command::new("cmake")
                .current_dir(&build)
                .args(["-G", "Ninja", "-S"])
                .arg(&source)
                .args(&args),
        )
        .context("configuring CMake")?;
        fs::write(&configured, args.join("\n"))?;
    }
    crate::port::run(Command::new("ninja").current_dir(&build).arg("--quiet"))
        .context("building CMake")?;
    // The install lists every file, also those that are up to date.
    crate::port::run(
        Command::new(build.join("bin/cmake"))
            .args(["--install", ".", "--strip"])
            .current_dir(&build)
            .stdout(Stdio::null()),
    )
    .context("installing CMake")?;
    Ok(install.join("bin/cmake"))
}
