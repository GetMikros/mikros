//! The pinned Meson release that builds ported C and C++ projects for
//! Mikros with the sysroot's cross file: fetching it and applying the
//! Mikros patches in `tools/meson/patches/`. Meson runs from its source
//! in `target/meson`, with the host's Python.

use crate::tools::Version;
use anyhow::Result;
use std::process::Command;

/// The pinned Meson release.
pub const MESON: Version = Version(1, 12, 1);
const URL: &str =
    "https://github.com/mesonbuild/meson/releases/download/1.12.1/meson-1.12.1.tar.gz";
/// SHA-256 of the release archive at `URL`.
const SHA256: &str = "ab0a6ca09f8ef70c564c8241fb5a23957886a0b53fb58412b5e07eaf07dba743";
const SOURCE: &str = "meson-1.12.1";

/// Fetches and patches Meson, and returns the command that runs it.
pub fn command() -> Result<Command> {
    let dir = crate::workspace_root().join("target/meson");
    let archive = dir.join(format!("{SOURCE}.tar.gz"));
    crate::fetch::cached(&format!("Meson {MESON} source"), URL, SHA256, &archive)?;
    let source = crate::port::patched_source(
        &archive,
        &dir,
        SOURCE,
        &crate::workspace_root().join("tools/meson/patches"),
        &[],
    )?;
    let mut command = Command::new("python3");
    command.arg(source.join("meson.py"));
    Ok(command)
}
