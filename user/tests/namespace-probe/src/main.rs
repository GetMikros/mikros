//! A program that the service manager starts as a boot service, from a
//! boot image that the process manager serves with `root=bootfs`, to
//! check the namespace that its manifest gives it. A copy started before
//! the process manager built namespaces from profiles, which has `/`,
//! starts another copy, and again until one has its profile, and ends as
//! that one does. The one with its profile checks that it has exactly
//! its manifest's entries and `/dev`, that the boot image and the
//! copies of its `etc` on tmpfs serve them with the rights given, and
//! that the writable entry that did not exist was made. It exits with 0
//! when every check passed, or with the number of the first that
//! failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::process::{LauncherClient, SpawnRequest, WaitOptions};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const BINARY: &str = "/usr/bin/namespace-probe";
/// The user of the caller, for `Spawn`.
const CALLER_USER: u32 = u32::MAX;
/// How long to wait before starting another copy.
const RETRY: i64 = 20_000_000;
/// How many copies to start before giving up.
const COPIES_MAX: u32 = 200;

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
    if entries.iter().any(|(path, _)| path == "/") {
        // A copy tells its starter that it came too early.
        if is_copy(&managed.header.arguments) {
            return EARLY;
        }
        return again(LauncherClient::new(managed.header.launcher));
    }
    match check(&entries) {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

/// Starts copies of the program until one does not end at once for
/// having been started too early, and returns its exit code.
fn again(launcher: LauncherClient) -> i64 {
    for _ in 0..COPIES_MAX {
        let mut now = 0;
        // SAFETY: the out pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        let deadline = now + RETRY;
        // SAFETY: the call takes no pointers.
        let _ = unsafe { sys::thread_sleep(deadline) };
        let Some(arguments) = arguments() else {
            return 2;
        };
        let request = SpawnRequest {
            path: BINARY.into(),
            arguments,
            handles: Vec::new(),
            app_id: None,
            user: CALLER_USER,
            login_session: false,
        };
        let Ok(Ok(spawned)) = launcher.spawn(request, FOREVER) else {
            return 3;
        };
        let Ok(Ok(waited)) = launcher.wait(spawned.pid as i32, WaitOptions(0), FOREVER, FOREVER)
        else {
            return 4;
        };
        if waited.status.exit_code != EARLY {
            return waited.status.exit_code;
        }
    }
    5
}

/// What a copy that has `/` would exit with, were it not to start another.
const EARLY: i64 = 99;

/// The arguments of a copy: the program's path and `copy`.
fn arguments() -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for arg in [BINARY, "copy"] {
        bytes.extend_from_slice(arg.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(4096, 0, &mut mem) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer describes `bytes`.
    let written = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    (written == mikros_rt::OK).then_some(mem)
}

/// Whether the arguments in `mem` are those of a copy: two of them.
fn is_copy(mem: &OwnedHandle) -> bool {
    let mut count = [0u8; 4];
    // SAFETY: the pointer describes `count`.
    let read = unsafe { sys::mem_read(mem.raw(), 0, count.as_mut_ptr(), 4) };
    read == mikros_rt::OK && u32::from_le_bytes(count) == 2
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
    let write = open(
        share,
        "data.txt",
        OpenFlags(0),
        Rights(Rights::READ.0 | Rights::WRITE.0),
    );
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
    let write = open(
        etc,
        "probe.conf",
        OpenFlags(0),
        Rights(Rights::READ.0 | Rights::WRITE.0),
    );
    expect(matches!(write, Err(Some(Error::AccessDenied))), 15)?;
    // The writable entry was made, and takes files.
    let state = entry("/var/lib/namespace-probe").ok_or(16)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let file = open(
        state,
        "state",
        create,
        Rights(Rights::READ.0 | Rights::WRITE.0),
    )
    .map_err(|_| 16)?;
    expect(
        file.write(0, b"ok".to_vec(), FOREVER)
            .ok()
            .and_then(|r| r.ok())
            .is_some(),
        16,
    )?;
    Ok(())
}
