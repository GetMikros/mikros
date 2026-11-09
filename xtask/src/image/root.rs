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

/// The top-level directories whose contents the boot image holds at the
/// same paths, which the root file system gets too.
const SHARED_DIRS: [&str; 5] = ["etc", "var", "Users", "Apps", "Libraries"];

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

/// The directories a live system's boot image holds: the system at its
/// namespace path under `usr`, and the top-level directories that the
/// process manager fills from the boot image.
pub fn live_dirs() -> Vec<String> {
    let system = SYSTEM_DIRS.iter().map(|d| format!("usr/{d}"));
    let top = ["etc", "var", "Users", "Apps", "Libraries"];
    core::iter::once("usr".to_owned())
        .chain(system)
        .chain(top.map(str::to_owned))
        .collect()
}

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

/// Copies the members of the archive `boot_image` into the tree `dir`
/// at their paths on the root file system: those under `usr` into the
/// system version `version`, and those under the shared top-level
/// directories at the same paths. Members elsewhere, such as `init`,
/// are left out.
fn copy_boot_image(dir: &Path, version: &str, boot_image: &Path) -> Result<()> {
    let file =
        File::open(boot_image).with_context(|| format!("opening {}", boot_image.display()))?;
    let mut archive = tar::Archive::new(file);
    let system = dir.join("@system").join(version);
    for entry in archive.entries().context("reading the boot image")? {
        let mut entry = entry.context("reading the boot image")?;
        let path = entry.path()?.into_owned();
        let target = match path.strip_prefix("usr") {
            Ok(rest) => system.join(rest),
            Err(_) => match path.components().next() {
                Some(top) if SHARED_DIRS.iter().any(|d| top.as_os_str() == *d) => dir.join(&path),
                _ => continue,
            },
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        match entry.header().entry_type() {
            tar::EntryType::Directory => fs::create_dir_all(&target)?,
            tar::EntryType::Regular => {
                entry
                    .unpack(&target)
                    .with_context(|| format!("writing {}", target.display()))?;
            }
            tar::EntryType::Symlink => {
                let link = entry.link_name()?.context("a link without a target")?;
                symlink_to(&link, &target)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_to(target: &Path, link: &Path) -> Result<()> {
    Ok(std::os::unix::fs::symlink(target, link)?)
}

#[cfg(not(unix))]
fn symlink_to(_target: &Path, _link: &Path) -> Result<()> {
    bail!("creating the root file system needs a Linux host")
}

/// Creates the root file system for `kind`, holding the system of
/// `boot_image`, and returns the path of its image.
pub fn create(kind: Kind, boot_image: &Path) -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/image");
    let tree = dir.join(format!("root-{}", kind.name()));
    if tree.exists() {
        fs::remove_dir_all(&tree).with_context(|| format!("removing {}", tree.display()))?;
    }
    let version = version(kind);
    write_tree(&tree, &version)?;
    copy_boot_image(&tree, &version, boot_image)?;

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
    fn lists_the_live_system_at_its_paths() {
        let dirs = live_dirs();
        assert_eq!(dirs[0], "usr");
        for d in [
            "usr/bin",
            "usr/lib/services",
            "usr/share",
            "etc",
            "var",
            "Users",
            "Apps",
            "Libraries",
        ] {
            assert!(dirs.iter().any(|x| x == d), "{d}");
        }
        assert!(
            !dirs
                .iter()
                .any(|d| d.contains('@') || d == "tmp" || d == "Volumes")
        );
    }

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
    fn copies_the_system_of_the_boot_image() {
        let dir = std::env::temp_dir().join(format!("xtask-copy-{}", std::process::id()));
        let tree = dir.join("tree");
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("boot.img");
        let mut archive = tar::Builder::new(File::create(&image).unwrap());
        let mut add = |name: &str, data: &[u8]| {
            let mut header = tar::Header::new_ustar();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            archive.append_data(&mut header, name, data).unwrap();
        };
        add("init", b"init");
        add("usr/bin/tool", b"tool");
        add("usr/lib/services/s.yaml", b"name: s");
        add("etc/config", b"config");
        add("tests/data", b"data");
        archive.finish().unwrap();
        drop(archive);
        write_tree(&tree, "v1").unwrap();
        copy_boot_image(&tree, "v1", &image).unwrap();
        assert_eq!(fs::read(tree.join("@system/v1/bin/tool")).unwrap(), b"tool");
        assert_eq!(
            fs::read(tree.join("@system/v1/lib/services/s.yaml")).unwrap(),
            b"name: s"
        );
        assert_eq!(fs::read(tree.join("etc/config")).unwrap(), b"config");
        assert!(!tree.join("init").exists() && !tree.join("tests").exists());
        assert!(!tree.join("usr").exists());
        fs::remove_dir_all(&dir).unwrap();
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
