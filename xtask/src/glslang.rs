//! The pinned glslang release, whose `glslangValidator` compiles the
//! GLSL shaders that Mesa builds into its Vulkan drivers to SPIR-V on the
//! build host: fetching its source, and building and installing it in
//! `target/glslang/install` with the host's CMake and compilers.

use crate::tools::Version;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The pinned glslang release.
pub const GLSLANG: Version = Version(16, 6, 0);
const URL: &str = "https://github.com/KhronosGroup/glslang/archive/refs/tags/16.6.0.tar.gz";
/// SHA-256 of the source archive at `URL`.
const SHA256: &str = "9c09b901149c729df745057dafa815278aaa101b84d2b6e14f16a42de52f97f2";
const SOURCE: &str = "glslang-16.6.0";
/// The options of the build: the validator alone, without the SPIR-V
/// optimizer, which needs SPIRV-Tools, or tests.
const OPTIONS: [&str; 8] = [
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_C_COMPILER=clang",
    "-DCMAKE_CXX_COMPILER=clang++",
    "-DENABLE_OPT=OFF",
    "-DGLSLANG_TESTS=OFF",
    "-DGLSLANG_ENABLE_INSTALL=ON",
    "-DENABLE_HLSL=OFF",
    "-DBUILD_SHARED_LIBS=OFF",
];

fn dir() -> PathBuf {
    crate::workspace_root().join("target/glslang")
}

/// Fetches, builds and installs glslang, and returns the directory of
/// the installed `glslangValidator`. A build that is up to date only runs
/// Ninja.
pub fn build() -> Result<PathBuf> {
    let archive = dir().join(format!("{SOURCE}.tar.gz"));
    crate::fetch::cached(&format!("glslang {GLSLANG} source"), URL, SHA256, &archive)?;
    let build = dir().join("build");
    let source = crate::port::patched_source(
        &archive,
        &dir(),
        SOURCE,
        &crate::workspace_root().join("tools/glslang/patches"),
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
        .context("configuring glslang")?;
        fs::write(&configured, args.join("\n"))?;
    }
    crate::port::run(
        Command::new("ninja")
            .current_dir(&build)
            .args(["--quiet", "install"])
            .stdout(Stdio::null()),
    )
    .context("building glslang")?;
    Ok(install.join("bin"))
}

/// A Meson native file that names the `glslangValidator` that `build`
/// installed, written beside its installation, for projects that compile
/// shaders on the build host.
pub fn native_file() -> Result<PathBuf> {
    let bin = build()?;
    let file = dir().join("glslang.meson");
    let text = format!(
        "# glslang for the build host, written by cargo xtask.\n\n[binaries]\nglslangValidator = '{}'\n",
        bin.join("glslangValidator").display()
    );
    if fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(file)
}
