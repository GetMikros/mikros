//! As the btrfs test driver, after its other checks: whether the root's
//! file system keeps working, without losing writes, when the driver of
//! the disk it lies on is killed and restarts. The file systems of the
//! disk's partitions go on through the partition driver, which keeps
//! their requests until the restarted driver gives the disk again.

use crate::fat::{named, ok, open};
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::OpenFlags;
use mikros_rt::interfaces::mikros::process::LauncherAsyncClient;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

/// The app ID of the driver of the disk that the root lies on.
const DISK_DRIVER: &str = "system.driver.virtioblk";
const KILL: u32 = 9;
/// The file written across the restart, in pieces of `PIECE` bytes:
/// `BEFORE` before the kill and `AFTER` after it, each synced.
const FILE: &str = "crash-test";
const PIECE: usize = 16384;
const BEFORE: u32 = 4;
const AFTER: u32 = 12;

/// Says what failed, through the log service.
fn failed(what: &str) -> Option<()> {
    mikros_rt::log!(Error, "test-btrfs: {what} failed");
    None
}

/// The bytes of piece `i` of the file.
fn piece(i: u32) -> Vec<u8> {
    (0..PIECE)
        .map(|at| (at as u32 ^ i.wrapping_mul(0x9E37)) as u8)
        .collect()
}

/// Writes pieces of a file on the root and syncs each, kills the disk's
/// driver through `launcher`, writes and syncs the rest, which waits
/// until the driver is back, and reads the whole file back.
pub async fn root_survives(
    directory: &DirectoryAsyncClient,
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
) -> Option<()> {
    let Some(root) = named(directory, reactor, "root").await else {
        return failed("connecting to the root");
    };
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let Ok((file, _)) = open(&root, FILE, flags, reactor).await else {
        return failed("creating the file");
    };
    let write = async |i: u32| -> Option<()> {
        let at = u64::from(i) * PIECE as u64;
        let written = ok(file.write(at, piece(i)).await);
        if written.is_none_or(|w| w.written as usize != PIECE) {
            return failed("writing");
        }
        if ok(file.sync().await).is_none() {
            return failed("syncing");
        }
        Some(())
    };
    for i in 0..BEFORE {
        write(i).await?;
    }

    // The process manager shows the processes of the caller's user, as
    // drivers all are.
    let mut disk_driver = None;
    let Ok(listed) = launcher.list().await else {
        return failed("listing the processes");
    };
    for pid in listed.pids {
        let Ok(Ok(info)) = launcher.get_info(pid).await else {
            continue;
        };
        if info.info.app_id == DISK_DRIVER {
            if disk_driver.is_some() {
                return failed("finding one disk driver");
            }
            disk_driver = Some(pid);
        }
    }
    let Some(disk_driver) = disk_driver else {
        return failed("finding the disk driver");
    };
    if let Ok(Err(status)) | Err(status) = launcher
        .kill(disk_driver, KILL)
        .await
        .map_err(|_| mikros_rt::interfaces::mikros::base::Status::PeerClosed)
    {
        mikros_rt::log!(
            Error,
            "test-btrfs: killing pid {disk_driver} failed: {status:?}"
        );
        return None;
    }

    for i in BEFORE..BEFORE + AFTER {
        write(i).await?;
    }
    for i in 0..BEFORE + AFTER {
        let at = u64::from(i) * PIECE as u64;
        let mut read = Vec::new();
        while read.len() < PIECE {
            let want = (PIECE - read.len()) as u32;
            let data = ok(file.read(at + read.len() as u64, want).await)?.data;
            if data.is_empty() {
                return failed("reading the file back");
            }
            read.extend_from_slice(&data);
        }
        if read != piece(i) {
            return failed("finding what was written");
        }
    }
    match ok(root.unlink(FILE.into(), false).await) {
        Some(_) => Some(()),
        None => failed("removing the file"),
    }
}
