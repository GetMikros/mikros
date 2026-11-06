//! What building a ported C or C++ project needs: its Mikros patches,
//! a digest of them, and running its build tools.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
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

/// The file in a patched source that records which patches it has.
const STAMP: &str = ".mikros-patches";

/// The source `dir/<source>`, extracted from the gzip-compressed tar
/// `archive`, with every patch in `patches` applied. It is extracted
/// again whenever the patches changed, and the directories `stale`,
/// built from the older source, are removed then.
pub fn patched_source(
    archive: &Path,
    dir: &Path,
    source: &str,
    patches: &Path,
    stale: &[&Path],
) -> Result<PathBuf> {
    let patches = self::patches(patches)?;
    let stamp = self::stamp(&patches)?;
    let source = dir.join(source);
    if fs::read_to_string(source.join(STAMP)).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(source);
    }
    for stale in std::iter::once(&source.as_path()).chain(stale) {
        if stale.exists() {
            fs::remove_dir_all(stale).with_context(|| format!("removing {}", stale.display()))?;
        }
    }
    println!("extracting {}", archive.display());
    let gz = flate2::read::GzDecoder::new(File::open(archive)?);
    tar::Archive::new(gz)
        .unpack(dir)
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
