// SPDX-License-Identifier: GPL-2.0
//! The btrfs code on the shim, run on the build host.

mod memory;
mod reads;
mod shares;
mod subvolumes;
mod writes;

use crate::linux::host::Disk;
use std::format;
use std::sync::OnceLock;
use std::vec::Vec;

/// Starts the shim, once for all the tests, which share it.
fn start() {
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    assert_eq!(*STARTED.get_or_init(crate::linux::start), Ok(()));
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

/// Writable filesystems mounted at once, whose threads are more than the
/// shim's 256 CPUs, which they hold only while they do not sleep.
const MOUNTS: usize = 100;

#[test]
fn mounts_more_filesystems_than_the_shim_has_cpus_for() {
    start();
    let mounts: std::vec::Vec<crate::linux::Mount> = (0..MOUNTS)
        .map(|i| {
            let name = format!("many-mounts-{i}");
            let disk = std::ffi::CString::new(name.as_str()).unwrap();
            Disk::add(&disk, image(&name, 256 << 20, &[]), false).unwrap();
            let source = std::ffi::CString::new(format!("/dev/{name}")).unwrap();
            crate::linux::Mount::new(&source, false, None).unwrap()
        })
        .collect();
    for mount in &mounts {
        mount.sync().unwrap();
    }
}

/// Bytes that do not compress, from xorshift64 seeded with `seed`.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Text that compresses well.
fn text(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut line = 0;
    while out.len() < len {
        out.extend_from_slice(format!("line {line} of a file that compresses well\n").as_bytes());
        line += 1;
    }
    out.truncate(len);
    out
}

/// A fresh btrfs of `size` bytes from the host's mkfs.btrfs, with its
/// options `options`, as a file that is already unlinked.
fn image(name: &str, size: u64, options: &[&std::ffi::OsStr]) -> std::fs::File {
    let image = Image::new(name, size, options);
    let file = image.open();
    drop(image);
    file
}

/// A fresh btrfs of the host's mkfs.btrfs in a file, which is removed
/// when this is dropped.
struct Image(std::path::PathBuf);

impl Image {
    fn new(name: &str, size: u64, options: &[&std::ffi::OsStr]) -> Self {
        let path = std::env::temp_dir().join(format!("mikros-btrfs-{name}-{}", std::process::id()));
        let image = Image(path);
        let file = std::fs::File::create_new(&image.0).unwrap();
        file.set_len(size).unwrap();
        let status = std::process::Command::new("mkfs.btrfs")
            .arg("--quiet")
            .args(options)
            .arg(&image.0)
            .status()
            .expect("the tests need mkfs.btrfs of btrfs-progs");
        assert!(status.success());
        image
    }

    fn open(&self) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.0)
            .unwrap()
    }

    /// Asserts that the host's btrfs check finds nothing wrong with the
    /// file system, its data's checksums included.
    fn check(&self) {
        let output = std::process::Command::new("btrfs")
            .args(["check", "--readonly", "--check-data-csum"])
            .arg(&self.0)
            .output()
            .expect("the tests need btrfs of btrfs-progs");
        assert!(
            output.status.success(),
            "btrfs check: {}{}",
            std::string::String::from_utf8_lossy(&output.stdout),
            std::string::String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The magic number of btrfs.
const BTRFS_SUPER_MAGIC: u64 = 0x9123_683e;

#[test]
fn mounts_read_only() {
    start();
    Disk::add(
        c"mounts-read-only",
        image("mounts-read-only", 256 << 20, &[]),
        true,
    )
    .unwrap();
    let mount = crate::linux::Mount::new(c"/dev/mounts-read-only", true, None).unwrap();
    let statfs = mount.statfs().unwrap();
    assert_eq!(statfs.kind, BTRFS_SUPER_MAGIC);
    assert_eq!(statfs.block_size, 4096);
    assert_eq!(statfs.name_max, 255);
    assert!(statfs.blocks > 0 && statfs.blocks_free <= statfs.blocks);
}
