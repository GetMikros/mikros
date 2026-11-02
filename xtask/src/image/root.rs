//! The btrfs file system of the root partition.

use crate::build::Kind;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The size the file system is created with. Files are sparse, so unused
/// space takes no room on the host. mkfs.btrfs grows the file when the tree
/// does not fit.
const SIZE: u64 = 1 << 30;

/// The name of the system version the image holds.
pub fn version(kind: Kind) -> String {
    format!("{}-{}", env!("CARGO_PKG_VERSION"), kind.name())
}

/// The directories of the system image.
const SYSTEM_DIRS: [&str; 6] = [
    "bin",
    "lib",
    "lib/drivers",
    "lib/services",
    "include",
    "share",
];

/// The directories at the top level, created as directories or as
/// subvolumes (see `subvolumes`).
const TOP_DIRS: [&str; 9] = [
    "etc",
    "var",
    "Libraries",
    "Apps",
    "Users",
    "tmp",
    "boot/efi",
    "Volumes",
    "@system",
];

/// The `--subvol` values for the tree written by `write_tree`.
fn subvolumes(version: &str) -> Vec<String> {
    vec![
        "@system".to_owned(),
        format!("ro:@system/{version}"),
        "etc".to_owned(),
        "var".to_owned(),
        "Libraries".to_owned(),
    ]
}

/// Writes the directory tree of the root file system into `dir`, which must
/// not exist.
fn write_tree(dir: &Path, version: &str) -> Result<()> {
    for top in TOP_DIRS {
        fs::create_dir_all(dir.join(top))?;
    }
    let system = dir.join("@system");
    for sub in SYSTEM_DIRS {
        fs::create_dir_all(system.join(version).join(sub))?;
    }
    symlink(version, &system.join("current"))
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> Result<()> {
    Ok(std::os::unix::fs::symlink(target, link)?)
}

#[cfg(not(unix))]
fn symlink(_target: &str, _link: &Path) -> Result<()> {
    bail!("creating the root file system needs a Linux host")
}

/// The mkfs.btrfs arguments that create the file system in `image` from
/// `tree`.
fn mkfs_args(tree: &Path, image: &Path, version: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-q".into(), "-f".into(), "--rootdir".into(), tree.into()];
    for subvolume in subvolumes(version) {
        args.push("--subvol".into());
        args.push(subvolume.into());
    }
    args.push(image.into());
    args
}

/// Creates the root file system for `kind` and returns the path of its
/// image.
pub fn create(kind: Kind) -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/image");
    let tree = dir.join(format!("root-{}", kind.name()));
    if tree.exists() {
        fs::remove_dir_all(&tree).with_context(|| format!("removing {}", tree.display()))?;
    }
    let version = version(kind);
    write_tree(&tree, &version)?;

    let image = dir.join(format!("root-{}.btrfs", kind.name()));
    File::create(&image)?.set_len(SIZE)?;
    let status = Command::new("mkfs.btrfs")
        .args(mkfs_args(&tree, &image, &version))
        .status()
        .context("failed to run mkfs.btrfs")?;
    if !status.success() {
        bail!("mkfs.btrfs failed with {status}");
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_version_after_the_workspace_and_kind() {
        assert_eq!(version(Kind::Debug), "0.0.0-debug");
        assert_eq!(version(Kind::Production), "0.0.0-production");
    }

    #[test]
    fn makes_the_system_version_a_read_only_subvolume() {
        let args = mkfs_args(Path::new("tree"), Path::new("root.btrfs"), "1.0-debug");
        let args: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "-q",
                "-f",
                "--rootdir",
                "tree",
                "--subvol",
                "@system",
                "--subvol",
                "ro:@system/1.0-debug",
                "--subvol",
                "etc",
                "--subvol",
                "var",
                "--subvol",
                "Libraries",
                "root.btrfs"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_the_tree_without_usr() {
        let dir = std::env::temp_dir().join(format!("xtask-root-{}", std::process::id()));
        write_tree(&dir, "v1").unwrap();
        assert!(dir.join("@system/v1/lib/services").is_dir());
        assert_eq!(
            fs::read_link(dir.join("@system/current")).unwrap(),
            Path::new("v1")
        );
        assert!(dir.join("boot/efi").is_dir() && dir.join("etc").is_dir());
        assert!(!dir.join("usr").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
