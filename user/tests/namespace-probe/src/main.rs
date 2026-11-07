//! A program that the service manager starts as a boot service, from a
//! boot image that the process manager serves with `root=bootfs`, to
//! check the namespace that its manifest gives it. Started after tmpfs,
//! it has its profile from the start: exactly its manifest's entries and
//! `/dev`, which the boot image and the copies of its `etc` on tmpfs
//! serve with the rights given, and the writable entry that did not
//! exist was made. It exits with 0 when every check passed, or with the
//! number of the first that failed.

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

fn check(entries: &[(String, NodeClient)]) -> Result<(), i64> {
    let paths: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
    expect(
        paths
            == [
                "/usr/share/probe",
                "/var/lib/namespace-probe",
                "/etc",
                "/dev",
            ],
        10,
    )?;
    let entry = |path: &str| entries.iter().find(|(p, _)| p == path).map(|(_, n)| n);
    // The boot image serves /usr, read only.
    let share = entry("/usr/share/probe").ok_or(11)?;
    expect(read(share, "data.txt").as_deref() == Some(&b"data"[..]), 11)?;
    let write = open(share, "data.txt", OpenFlags(0), RW);
    expect(matches!(write, Err(Some(Error::AccessDenied))), 12)?;
    // /etc is tmpfs's copy of the boot image's, here read only.
    let etc = entry("/etc").ok_or(13)?;
    expect(
        read(etc, "probe.conf").as_deref() == Some(&b"probe"[..]),
        13,
    )?;
    let info = etc
        .query_filesystem(FOREVER)
        .ok()
        .and_then(|r| r.ok())
        .ok_or(14)?;
    expect(info.info.r#type == "tmpfs", 14)?;
    let write = open(etc, "probe.conf", OpenFlags(0), RW);
    expect(matches!(write, Err(Some(Error::AccessDenied))), 15)?;
    // The writable entry was made, and takes files.
    let state = entry("/var/lib/namespace-probe").ok_or(16)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let file = open(state, "state", create, RW).map_err(|_| 16)?;
    expect(
        file.write(0, b"ok".to_vec(), FOREVER)
            .ok()
            .and_then(|r| r.ok())
            .is_some(),
        16,
    )?;
    Ok(())
}
