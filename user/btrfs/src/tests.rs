// SPDX-License-Identifier: GPL-2.0
//! The btrfs code on the shim, run on the build host.

use crate::host::Disk;
use std::format;
use std::sync::OnceLock;

/// Starts the shim, once for all the tests, which share it.
fn start() {
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    assert_eq!(*STARTED.get_or_init(crate::start), Ok(()));
}

#[test]
fn starts() {
    start();
}

#[test]
fn adds_disks() {
    start();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("mikros-btrfs-adds-disks-{}", std::process::id()));
    let file = std::fs::File::create_new(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    file.set_len(1 << 20).unwrap();
    assert_eq!(Disk::add(c"adds-disks", file, false), Ok(()));
}

/// A fresh btrfs of `size` bytes from the host's mkfs.btrfs, as a
/// file that is already unlinked.
fn image(name: &str, size: u64) -> std::fs::File {
    let path = std::env::temp_dir().join(format!("mikros-btrfs-{name}-{}", std::process::id()));
    let file = std::fs::File::create_new(&path).unwrap();
    file.set_len(size).unwrap();
    let status = std::process::Command::new("mkfs.btrfs")
        .arg("--quiet")
        .arg(&path)
        .status()
        .expect("the tests need mkfs.btrfs of btrfs-progs");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(status.success());
    file
}

/// The magic number of btrfs.
const BTRFS_SUPER_MAGIC: u64 = 0x9123_683e;

#[test]
fn mounts_read_only() {
    start();
    Disk::add(
        c"mounts-read-only",
        image("mounts-read-only", 256 << 20),
        true,
    )
    .unwrap();
    let mount = crate::Mount::new(c"/dev/mounts-read-only", true, None).unwrap();
    let statfs = mount.statfs().unwrap();
    assert_eq!(statfs.kind, BTRFS_SUPER_MAGIC);
    assert_eq!(statfs.block_size, 4096);
    assert_eq!(statfs.name_max, 255);
    assert!(statfs.blocks > 0 && statfs.blocks_free <= statfs.blocks);
}
