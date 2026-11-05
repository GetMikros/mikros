//! What building a ported C or C++ project needs: its Mikros patches,
//! a digest of them, and running its build tools.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The patches in `dir`, in the order they apply.
pub fn patches(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut patches: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    patches.retain(|p| p.extension().is_some_and(|e| e == "patch"));
    patches.sort();
    Ok(patches)
}

/// A digest of the names and contents of `patches`.
pub fn stamp(patches: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for patch in patches {
        hasher.update(patch.file_name().unwrap().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(fs::read(patch)?);
        hasher.update([0]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Runs `command`, and fails unless it succeeds.
pub fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {:?}", command.get_program()))?;
    if !status.success() {
        bail!("{:?} failed with {status}", command.get_program());
    }
    Ok(())
}
