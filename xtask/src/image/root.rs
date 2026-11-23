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

/// The directories of a system version (MIKSPEC-0100.R2), with the empty
/// folders at which the writable entries appear.
const SYSTEM_DIRS: [&str; 13] = [
    "bin",
    "services",
    "drivers",
    "programs",
    "libraries",
    "share",
    "include",
    "utilities",
    "config",
    "state",
    "logs",
    "tmp",
    "boot/efi",
];

/// The subvolumes at the top level beside `@system` (MIKSPEC-0100.R4),
/// whose contents an archive holds at the same paths. Each folder directly
/// below those of `PER_FOLDER` is a subvolume of its own.
const SHARED_DIRS: [&str; 8] = [
    "config",
    "state",
    "logs",
    "drivers",
    "libraries",
    "apps",
    "public",
    "users",
];
const PER_FOLDER: [&str; 5] = ["drivers", "libraries", "apps", "public", "users"];

/// The name of the version that `@system/previous` names in a root file
/// system that `create` makes with one: a copy of the current version.
pub fn previous_version(kind: Kind) -> String {
    format!("{}-previous", version(kind))
}

/// The `--subvol` values for the tree written by `write_tree` with the
/// system versions `versions`, and with the folders `folders` directly
/// below those of `PER_FOLDER`, by their paths from the top.
fn subvolumes(versions: &[&str], folders: &[String]) -> Vec<String> {
    let mut all = vec!["@system".to_owned()];
    all.extend(versions.iter().map(|v| format!("ro:@system/{v}")));
    all.extend(SHARED_DIRS.map(str::to_owned));
    all.extend(folders.iter().cloned());
    all
}

/// The folders directly below those of `PER_FOLDER` in the tree `dir`,
/// by their paths from the top, sorted.
fn folders(dir: &Path) -> Result<Vec<String>> {
    let mut all = Vec::new();
    for top in PER_FOLDER {
        for entry in fs::read_dir(dir.join(top))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let name = name.to_str().context("a folder name is not UTF-8")?;
                all.push(format!("{top}/{name}"));
            }
        }
    }
    all.sort();
    Ok(all)
}

/// Writes the directory tree of the root file system into `dir`, which must
/// not exist.
fn write_tree(dir: &Path, version: &str) -> Result<()> {
    for top in SHARED_DIRS.iter().chain(&["@system"]) {
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
/// `tree`, with crc32c checksums, which the bootloader checks.
fn mkfs_args(tree: &Path, image: &Path, versions: &[&str], folders: &[String]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-q".into(), "-f".into(), "--csum".into(), "crc32c".into()];
    args.extend(["--rootdir".into(), tree.into()]);
    for subvolume in subvolumes(versions, folders) {
        args.push("--subvol".into());
        args.push(subvolume.into());
    }
    args.push(image.into());
    args
}

/// Where a member of an archive at `path` goes on the root file system:
/// into the system version, at its path without `system/` when it is in
/// `system`, and otherwise, outside the shared top-level directories, at
/// the same path; `None` for those in a shared directory.
fn version_path(path: &Path) -> Option<PathBuf> {
    if let Ok(rest) = path.strip_prefix("system") {
        return Some(rest.to_owned());
    }
    match path.components().next() {
        Some(top) if SHARED_DIRS.iter().any(|d| top.as_os_str() == *d) => None,
        _ => Some(path.to_owned()),
    }
}

/// Copies the members of the archive `boot_image` into the tree `dir`
/// at their paths on the root file system: into the system version
/// `version` as `version_path` gives, and those under the shared
/// top-level directories at the same paths.
fn copy_boot_image(dir: &Path, version: &str, boot_image: &Path) -> Result<()> {
    let file =
        File::open(boot_image).with_context(|| format!("opening {}", boot_image.display()))?;
    let mut archive = tar::Archive::new(file);
    let system = dir.join("@system").join(version);
    for entry in archive.entries().context("reading the boot image")? {
        let mut entry = entry.context("reading the boot image")?;
        let path = entry.path()?.into_owned();
        let target = match version_path(&path) {
            Some(path) => system.join(path),
            None => dir.join(&path),
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

/// The boot list naming the files and links of the archive `boot_image`
/// at their paths in the system version, in the order of the archive.
fn boot_list(boot_image: &Path) -> Result<String> {
    let file =
        File::open(boot_image).with_context(|| format!("opening {}", boot_image.display()))?;
    let mut archive = tar::Archive::new(file);
    let mut list = String::new();
    for entry in archive.entries().context("reading the boot image")? {
        let entry = entry.context("reading the boot image")?;
        let kind = entry.header().entry_type();
        if kind != tar::EntryType::Regular && kind != tar::EntryType::Symlink {
            continue;
        }
        let path = entry.path()?.into_owned();
        if let Some(path) = version_path(&path) {
            let path = path.to_str().context("a member path is not UTF-8")?;
            list.push_str(path);
            list.push('\n');
        }
    }
    Ok(list)
}

/// Copies the directory tree `from` to `to`, which must not exist, with
/// its symbolic links as they are.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_symlink() {
            symlink_to(&fs::read_link(&source)?, &target)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Copies the files `demos` into the system version in the tree `root`, each
/// to its path in the version, as a program that can be run. Fails if a file
/// is there already, as every file of the system is.
fn install_demos(root: &Path, demos: &[(String, PathBuf)]) -> Result<()> {
    for (at, source) in demos {
        let target = root.join(at);
        if target.symlink_metadata().is_ok() {
            bail!("{at} is in the system already, and a demo cannot replace it");
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &target).with_context(|| format!("copying {}", source.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// The size of the file system that holds `demos`: the usual size, and the
/// room for the files and the metadata that go with them.
fn size_with(demos: &[(String, PathBuf)]) -> u64 {
    let bytes: u64 = demos
        .iter()
        .map(|(_, p)| fs::metadata(p).map_or(0, |m| m.len()))
        .sum();
    SIZE + bytes + bytes / 8
}

/// Creates the root file system for `kind`, holding the files of the
/// archive `boot_image` and then the system of the archive `system`,
/// with `kernel` and the boot list of the files of `boot_image` in the
/// system version, and returns the path of its image. With `previous`,
/// `@system/previous` names a copy of that version, `previous_version`.
/// The files `demos` are in the system version, but not in the boot list.
pub fn create(
    kind: Kind,
    system: &Path,
    boot_image: &Path,
    kernel: &Path,
    previous: bool,
    demos: &[(String, PathBuf)],
) -> Result<PathBuf> {
    let dir = crate::workspace_root().join("target/image");
    let tree = dir.join(format!("root-{}", kind.name()));
    if tree.exists() {
        fs::remove_dir_all(&tree).with_context(|| format!("removing {}", tree.display()))?;
    }
    let version = version(kind);
    write_tree(&tree, &version)?;
    // The files that the boot list names are there whatever the system.
    if boot_image != system {
        copy_boot_image(&tree, &version, boot_image)?;
    }
    copy_boot_image(&tree, &version, system)?;
    let root = tree.join("@system").join(&version);
    install_demos(&root, demos)?;
    fs::copy(kernel, root.join("mikros"))
        .with_context(|| format!("copying {}", kernel.display()))?;
    fs::create_dir_all(root.join("boot"))?;
    fs::write(root.join("boot/modules"), boot_list(boot_image)?)?;
    let mut versions = vec![version.clone()];
    if previous {
        let name = previous_version(kind);
        copy_tree(&root, &tree.join("@system").join(&name))?;
        symlink(&name, &tree.join("@system/previous"))?;
        versions.push(name);
    }

    let image = dir.join(format!("root-{}.btrfs", kind.name()));
    File::create(&image)?.set_len(size_with(demos))?;
    let versions: Vec<&str> = versions.iter().map(String::as_str).collect();
    let status = Command::new("mkfs.btrfs")
        .args(mkfs_args(&tree, &image, &versions, &folders(&tree)?))
        .status()
        .context("failed to run mkfs.btrfs")?;
    if !status.success() {
        bail!("mkfs.btrfs failed with {status}");
    }
    // mkfs.btrfs keeps the owners of the build's files.
    super::owners::make_owned_by_0(&image)?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_the_demos_as_programs_and_refuses_to_replace_a_file() {
        let dir = std::env::temp_dir().join(format!("xtask-root-demos-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/bin")).unwrap();
        fs::write(dir.join("root/bin/msh"), b"shell").unwrap();
        fs::write(dir.join("demo"), b"demo").unwrap();
        let demos = vec![
            ("bin/demo".to_owned(), dir.join("demo")),
            ("share/a/b".to_owned(), dir.join("demo")),
        ];
        install_demos(&dir.join("root"), &demos).unwrap();
        assert_eq!(fs::read(dir.join("root/bin/demo")).unwrap(), b"demo");
        assert_eq!(fs::read(dir.join("root/share/a/b")).unwrap(), b"demo");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("root/bin/demo"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        let replacing = vec![("bin/msh".to_owned(), dir.join("demo"))];
        let error = install_demos(&dir.join("root"), &replacing).unwrap_err();
        assert!(error.to_string().contains("bin/msh"));
        assert_eq!(fs::read(dir.join("root/bin/msh")).unwrap(), b"shell");
    }

    #[test]
    fn makes_room_for_the_demos() {
        let dir = std::env::temp_dir().join(format!("xtask-root-size-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("big"), vec![0u8; 8000]).unwrap();
        assert_eq!(size_with(&[]), SIZE);
        let demos = vec![("bin/big".to_owned(), dir.join("big"))];
        assert_eq!(size_with(&demos), SIZE + 8000 + 1000);
    }

    #[test]
    fn names_the_version_after_the_workspace_and_kind() {
        assert_eq!(version(Kind::Debug), "0.0.0-debug");
        assert_eq!(version(Kind::Production), "0.0.0-production");
        assert_eq!(version(Kind::Diag), "0.0.0-diag");
    }

    #[test]
    fn makes_the_system_version_a_read_only_subvolume() {
        let versions = ["1.0-debug", "0.9-debug"];
        let folders = ["users/ada".to_owned()];
        let args = mkfs_args(
            Path::new("tree"),
            Path::new("root.btrfs"),
            &versions,
            &folders,
        );
        let args: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "-q",
                "-f",
                "--csum",
                "crc32c",
                "--rootdir",
                "tree",
                "--subvol",
                "@system",
                "--subvol",
                "ro:@system/1.0-debug",
                "--subvol",
                "ro:@system/0.9-debug",
                "--subvol",
                "config",
                "--subvol",
                "state",
                "--subvol",
                "logs",
                "--subvol",
                "drivers",
                "--subvol",
                "libraries",
                "--subvol",
                "apps",
                "--subvol",
                "public",
                "--subvol",
                "users",
                "--subvol",
                "users/ada",
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
        add("system/bin/tool", b"tool");
        add("system/services/system.s/service.yaml", b"name: s");
        add("config/hostname", b"config");
        add("users/ada/documents/a", b"a");
        add("tests/data", b"data");
        let mut link = tar::Header::new_ustar();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        archive
            .append_link(&mut link, "system/bin/alias", "tool")
            .unwrap();
        archive.finish().unwrap();
        drop(archive);
        write_tree(&tree, "v1").unwrap();
        copy_boot_image(&tree, "v1", &image).unwrap();
        assert_eq!(
            boot_list(&image).unwrap(),
            "init\nbin/tool\nservices/system.s/service.yaml\ntests/data\nbin/alias\n"
        );
        assert_eq!(fs::read(tree.join("@system/v1/bin/tool")).unwrap(), b"tool");
        assert_eq!(
            fs::read(tree.join("@system/v1/services/system.s/service.yaml")).unwrap(),
            b"name: s"
        );
        assert_eq!(fs::read(tree.join("config/hostname")).unwrap(), b"config");
        assert_eq!(folders(&tree).unwrap(), ["users/ada"]);
        assert_eq!(fs::read(tree.join("@system/v1/init")).unwrap(), b"init");
        assert_eq!(
            fs::read(tree.join("@system/v1/tests/data")).unwrap(),
            b"data"
        );
        assert!(!tree.join("init").exists() && !tree.join("tests").exists());
        assert!(!tree.join("system").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writes_the_tree_of_subvolumes_and_the_version() {
        let dir = std::env::temp_dir().join(format!("xtask-root-{}", std::process::id()));
        write_tree(&dir, "v1").unwrap();
        for sub in [
            "services", "drivers", "config", "state", "logs", "tmp", "boot/efi",
        ] {
            assert!(dir.join("@system/v1").join(sub).is_dir(), "{sub}");
        }
        assert_eq!(
            fs::read_link(dir.join("@system/current")).unwrap(),
            Path::new("v1")
        );
        let mut top: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        top.sort();
        assert_eq!(
            top,
            [
                "@system",
                "apps",
                "config",
                "drivers",
                "libraries",
                "logs",
                "public",
                "state",
                "users"
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
