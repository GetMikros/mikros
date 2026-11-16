//! A program that the service manager starts as a boot service, from the
//! root partition of a live system, to check its namespace:
//! `/system/config` shows the root's files and takes changes, which go
//! to tmpfs, and `/system` is the root file system's, mounted read only. It exits with 0 when every
//! check passed, or with the number of the first that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeClient, OpenFlags, Rights};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
/// The file of the root's `/system/config`, what it holds there, and
/// what the probe writes over it.
const CONF: &str = "live-probe.conf";
const LOWER: &[u8] = b"lower\n";
const UPPER: &[u8] = b"upper\n";
/// A file that the probe makes in `/system/config`.
const MADE: &str = "live-probe.new";

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor, |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let entries: Vec<(String, NodeClient)> = managed
        .namespace
        .into_iter()
        .map(|e| (e.path, NodeClient::new(e.directory)))
        .collect();
    match check(&entries) {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

fn expect(condition: bool, code: i64) -> Result<(), i64> {
    if condition { Ok(()) } else { Err(code) }
}

/// Opens `path` from `dir` with `flags` and `rights`.
fn open(
    dir: &NodeClient,
    path: &str,
    flags: OpenFlags,
    rights: Rights,
) -> Result<NodeClient, Option<Error>> {
    let (node, server) = endpoints::<Node>().map_err(|_| None)?;
    match dir.open(path.into(), flags, 0o644, rights, server, FOREVER) {
        Ok(Ok(_)) => Ok(NodeClient::new(node)),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

fn read(dir: &NodeClient, path: &str) -> Option<Vec<u8>> {
    let file = open(dir, path, OpenFlags(0), Rights::READ).ok()?;
    file.read(0, 1000, FOREVER).ok()?.ok().map(|r| r.data)
}

/// Writes `data` to the file at `path` in `dir`, opened with `flags`.
fn write(dir: &NodeClient, path: &str, flags: OpenFlags, data: &[u8]) -> bool {
    let Ok(file) = open(dir, path, flags, RW) else {
        return false;
    };
    let written = file.write(0, data.to_vec(), FOREVER);
    matches!(written, Ok(Ok(_)))
}

/// Whether the file system that serves `dir` is read-only, and its type.
fn filesystem(dir: &NodeClient) -> Option<(bool, String)> {
    let info = dir.query_filesystem(FOREVER).ok()?.ok()?.info;
    Some((info.read_only, info.r#type))
}

fn check(entries: &[(String, NodeClient)]) -> Result<(), i64> {
    let entry = |path: &str| entries.iter().find(|(p, _)| p == path).map(|(_, n)| n);
    // The root file system serves /system, mounted read-only.
    let share = entry("/system/share/live-probe").ok_or(10)?;
    expect(filesystem(share) == Some((true, "btrfs".into())), 11)?;
    // /system/config shows the root's files, and takes changes.
    let etc = entry("/system/config").ok_or(12)?;
    expect(read(etc, CONF).as_deref() == Some(LOWER), 13)?;
    expect(filesystem(etc).is_some_and(|(read_only, _)| !read_only), 14)?;
    let over = OpenFlags(OpenFlags::TRUNCATE.0);
    expect(write(etc, CONF, over, UPPER), 15)?;
    expect(read(etc, CONF).as_deref() == Some(UPPER), 16)?;
    let new = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    expect(write(etc, MADE, new, b"made\n"), 17)?;
    expect(read(etc, MADE).as_deref() == Some(&b"made\n"[..]), 18)?;
    Ok(())
}
