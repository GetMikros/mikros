//! A boot service of the test image of the USB volumes run, which checks
//! that the FAT and btrfs volumes of a USB disk appear in `/Volumes`, can
//! be read and written there, and go away when the disk is unplugged.
//!
//! Started without `/Volumes`, it grants its own app ID and user
//! `files.volumes` through the broker and starts a copy of itself, which
//! has `/Volumes` in its namespace. For each of the volumes `USBSTICK`
//! and `USBBTRFS`, the copy waits for it, reads `HELLO.TXT`, which the
//! runner put there, writes `WRITTEN.TXT` and reads it back; it then logs
//! `READY`. The runner then unplugs the disk, and the copy logs `GONE`
//! once both volumes have left `/Volumes`. A failed check logs `FAILED`
//! with what failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision, Grant};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const BINARY: &str = "/usr/bin/volumes-probe";
const BROKER: &str = "mikros.permissions.Broker";
const VOLUMES_PERMISSION: &str = "files.volumes";
const VOLUMES: &str = "/Volumes";
/// The volumes' labels, and the files on them.
const LABELS: [&str; 2] = ["USBSTICK", "USBBTRFS"];
const HELLO: &str = "HELLO.TXT";
const HELLO_TEXT: &[u8] = b"hello from the host\n";
const WRITTEN: &str = "WRITTEN.TXT";
const WRITTEN_TEXT: &[u8] = b"written on Mikros\n";
/// The lines the runner waits for.
const READY: &str = "volumes-probe: read and wrote the USB volumes";
const GONE: &str = "volumes-probe: the USB volumes are gone";
/// The ID of the log handle among the user handles.
const LOG_ID: u32 = 2;
/// The user of the caller, for `Spawn`.
const CALLER_USER: u32 = u32::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
const MS: i64 = 1_000_000;
/// How long the volume may take to come, and to go.
const LIMIT: i64 = 60_000 * MS;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let log = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::User && h.id == LOG_ID)
        .map(|at| managed.handles.remove(at).handle);
    if let Some(log) = &log {
        mikros_rt::log::set_handle(log.raw());
    }
    let volumes = managed
        .namespace
        .into_iter()
        .find(|e| e.path == VOLUMES)
        .map(|e| NodeAsyncClient::new(e.directory, reactor.clone()));
    let checked = match volumes {
        Some(volumes) => executor.block_on(use_volumes(&volumes, &reactor)),
        None => {
            let header = managed.header;
            let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
            let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
            let started = start_copy(
                &directory,
                &launcher,
                &reactor,
                (header.user, &header.app_id),
                log,
            );
            executor.block_on(started)
        }
    };
    match checked {
        Ok(()) => 0,
        Err(what) => {
            mikros_rt::log!(Error, "volumes-probe: FAILED: {}", what);
            1
        }
    }
}

/// Grants `app_id` and `user` `files.volumes`, and starts a copy of the
/// probe, which gets `/Volumes` and the log handle `log`.
async fn start_copy(
    directory: &DirectoryAsyncClient,
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
    (user, app_id): (u32, &str),
    log: Option<OwnedHandle>,
) -> Result<(), &'static str> {
    let (client, server) = endpoints::<Broker>().map_err(|_| "making a channel")?;
    let connected = directory
        .connect(BROKER.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return Err("connecting to the broker");
    }
    let broker = BrokerAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
    let grant = Grant {
        user,
        app_id: app_id.into(),
        permission: VOLUMES_PERMISSION.into(),
        decision: Decision::Allowed,
    };
    if !matches!(broker.set_grant(grant).await, Ok(Ok(_))) {
        return Err("granting files.volumes");
    }
    let mut handles = Vec::new();
    if let Some(log) = log {
        let copy = duplicate(&log).ok_or("copying the log handle")?;
        handles.push(StartupHandle {
            role: HandleRole::User,
            id: LOG_ID,
            handle: copy,
        });
    }
    let request = SpawnRequest {
        path: BINARY.into(),
        arguments: arguments(BINARY).ok_or("making the arguments")?,
        handles,
        app_id: None,
        user: CALLER_USER,
        login_session: false,
    };
    match launcher.spawn(request).await {
        Ok(Ok(_)) => Ok(()),
        _ => Err("starting the copy"),
    }
}

/// Uses each volume in `volumes`, logs `READY`, and logs `GONE` once
/// they have left.
async fn use_volumes(volumes: &NodeAsyncClient, reactor: &Reactor) -> Result<(), &'static str> {
    for label in LABELS {
        use_volume(volumes, reactor, label).await?;
    }
    mikros_rt::log!(Info, "{}", READY);
    let limit = now() + LIMIT;
    while listed(volumes).await? {
        if now() >= limit {
            return Err("waiting for the volumes to go");
        }
        let _ = reactor.sleep_until(now() + 100 * MS).await;
    }
    mikros_rt::log!(Info, "{}", GONE);
    Ok(())
}

/// Waits for the volume `label` in `volumes`, reads `HELLO.TXT` on it,
/// and writes `WRITTEN.TXT` and reads it back.
async fn use_volume(
    volumes: &NodeAsyncClient,
    reactor: &Reactor,
    label: &str,
) -> Result<(), &'static str> {
    let hello = format!("{label}/{HELLO}");
    let written_path = format!("{label}/{WRITTEN}");
    let limit = now() + LIMIT;
    let file = loop {
        if let Ok(file) = open(volumes, reactor, &hello, OpenFlags(0), Rights::READ).await {
            break file;
        }
        if now() >= limit {
            return Err("waiting for a volume");
        }
        let _ = reactor.sleep_until(now() + 100 * MS).await;
    };
    let read = file.read(0, 4096).await;
    if !matches!(&read, Ok(Ok(r)) if r.data == HELLO_TEXT) {
        return Err("reading HELLO.TXT");
    }
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let written = open(volumes, reactor, &written_path, flags, RW).await?;
    let wrote = written.write(0, WRITTEN_TEXT.to_vec()).await;
    if !matches!(wrote, Ok(Ok(w)) if w.written as usize == WRITTEN_TEXT.len()) {
        return Err("writing WRITTEN.TXT");
    }
    if !matches!(written.sync().await, Ok(Ok(_))) {
        return Err("syncing WRITTEN.TXT");
    }
    let again = open(volumes, reactor, &written_path, OpenFlags(0), Rights::READ).await?;
    let read = again.read(0, 4096).await;
    if !matches!(&read, Ok(Ok(r)) if r.data == WRITTEN_TEXT) {
        return Err("reading WRITTEN.TXT back");
    }
    mikros_rt::log!(Info, "volumes-probe: read and wrote /Volumes/{}", label);
    Ok(())
}

/// Whether `/Volumes` still lists either volume.
async fn listed(volumes: &NodeAsyncClient) -> Result<bool, &'static str> {
    let mut cookie = 0;
    loop {
        let listed = match volumes.read_directory(cookie, false).await {
            Ok(Ok(listed)) => listed,
            _ => return Err("listing /Volumes"),
        };
        if listed.entries.is_empty() {
            return Ok(false);
        }
        if listed
            .entries
            .iter()
            .any(|e| LABELS.contains(&e.name.as_str()))
        {
            return Ok(true);
        }
        cookie = listed.next;
    }
}

/// Opens `path` below `dir` with `flags` and `rights`.
async fn open(
    dir: &NodeAsyncClient,
    reactor: &Reactor,
    path: &str,
    flags: OpenFlags,
    rights: Rights,
) -> Result<NodeAsyncClient, &'static str> {
    let (node, server) = endpoints::<Node>().map_err(|_| "making a channel")?;
    match dir.open(path.into(), flags, 0o644, rights, server).await {
        Ok(Ok(_)) => Ok(NodeAsyncClient::new(node, reactor.clone())),
        _ => Err("opening a file on the volume"),
    }
}

fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// The arguments of a program at `path`: only its path.
fn arguments(path: &str) -> Option<OwnedHandle> {
    let mut bytes = vec![];
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
