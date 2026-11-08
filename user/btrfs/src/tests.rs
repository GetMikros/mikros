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
