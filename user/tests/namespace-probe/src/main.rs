//! A program that the service manager starts as a boot service, from a
//! boot image that the process manager serves with `root=bootfs`, to
//! check the namespace that its manifest gives it. Started after tmpfs,
//! it has its profile from the start: exactly its manifest's entries and
//! `/dev`, which the boot image and the copies of its `etc` on tmpfs
//! serve with the rights given, and the writable entry that did not
//! exist was made. It then starts a copy of itself as a command-line
//! tool of user 0, which checks the entries of that profile, and a
//! native app of user 0, which cannot start. It exits with 0 when every
//! check passed, or with the number of the first that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::process::{LauncherClient, SpawnRequest, WaitOptions};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const BINARY: &str = "/usr/bin/namespace-probe";
/// The program of a native app's bundle, and the app's ID.
const NATIVE: &str = "/Apps/test.native/bin/hello";
const NATIVE_APP: &str = "test.native";
const CLI: &str = "system.cli";
/// The user of the caller, for `Spawn`.
const CALLER_USER: u32 = u32::MAX;
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
    let header = managed.header;
    let entries: Vec<(String, NodeClient)> = managed
        .namespace
        .into_iter()
        .map(|e| (e.path, NodeClient::new(e.directory)))
        .collect();
    let checked = match header.app_id == CLI {
        true => check_cli(&entries),
        false => check(&entries).and_then(|()| spawns(&LauncherClient::new(header.launcher))),
    };
    match checked {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

/// Starts `path` with `app_id` as the caller's user.
fn spawn(launcher: &LauncherClient, path: &str, app_id: &str) -> Result<u32, Option<Status>> {
    let arguments = arguments(path).ok_or(None)?;
    let request = SpawnRequest {
        path: path.into(),
        arguments,
        handles: Vec::new(),
        app_id: Some(app_id.into()),
        user: CALLER_USER,
        login_session: false,
    };
    match launcher.spawn(request, FOREVER) {
        Ok(Ok(spawned)) => Ok(spawned.pid),
        Ok(Err(status)) => Err(Some(status)),
        Err(_) => Err(None),
    }
}

/// A command-line tool of user 0 starts with its profile and passes its
/// checks; a native app of user 0 does not start.
fn spawns(launcher: &LauncherClient) -> Result<(), i64> {
    let pid = spawn(launcher, BINARY, CLI).map_err(|_| 20)?;
    let waited = launcher.wait(pid as i32, WaitOptions(0), FOREVER, FOREVER);
    let code = waited.ok().and_then(|w| w.ok()).ok_or(21)?.status.exit_code;
    expect(code == 0, code)?;
    let native = spawn(launcher, NATIVE, NATIVE_APP);
    expect(native == Err(Some(Status::AccessDenied)), 22)
}

/// The arguments of a program at `path`: only its path.
fn arguments(path: &str) -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
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
                "/usr/bin",
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

/// The entries of a command-line tool of user 0: `/usr`, `/etc`, `/Apps`
/// and `/Libraries` read only, its user's `/tmp` on tmpfs, and `/dev`.
fn check_cli(entries: &[(String, NodeClient)]) -> Result<(), i64> {
    let paths: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
    expect(
        paths == ["/usr", "/etc", "/Apps", "/Libraries", "/tmp", "/dev"],
        30,
    )?;
    let entry = |path: &str| entries.iter().find(|(p, _)| p == path).map(|(_, n)| n);
    let usr = entry("/usr").ok_or(31)?;
    expect(
        read(usr, "share/probe/data.txt").as_deref() == Some(&b"data"[..]),
        31,
    )?;
    let etc = entry("/etc").ok_or(32)?;
    expect(
        read(etc, "probe.conf").as_deref() == Some(&b"probe"[..]),
        32,
    )?;
    let write = open(etc, "probe.conf", OpenFlags(0), RW);
    expect(matches!(write, Err(Some(Error::AccessDenied))), 33)?;
    let apps = entry("/Apps").ok_or(34)?;
    expect(
        open(apps, "test.native", OpenFlags::DIRECTORY, Rights::READ).is_ok(),
        34,
    )?;
    let libraries = entry("/Libraries").ok_or(35)?;
    expect(
        read(libraries, "probe.txt").as_deref() == Some(&b"data"[..]),
        35,
    )?;
    let write = open(libraries, "probe.txt", OpenFlags(0), RW);
    expect(matches!(write, Err(Some(Error::AccessDenied))), 36)?;
    let tmp = entry("/tmp").ok_or(37)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let file = open(tmp, "cli", create, RW).map_err(|_| 37)?;
    let written = file.write(0, b"ok".to_vec(), FOREVER);
    expect(written.ok().and_then(|r| r.ok()).is_some(), 38)
}
