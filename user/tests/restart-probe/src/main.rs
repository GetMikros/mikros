//! The boot service of `tests/bus.img` and `tests/acpi-restart.img`,
//! which kills a bus driver below running drivers and checks that the
//! device manager runs it again while every other driver goes on as the
//! process it was, as a driver does that takes the restarted bus driver's
//! channel through `Reconnect`. Before the kill it waits until the
//! drivers have started. It logs `PASSED`, or `FAILED` with what failed.
//!
//! - As `system.restart-probe`, in the bus restart runs of `cargo xtask
//!   test`, with the root file system on a disk behind a virtio-blk, NVMe
//!   or AHCI controller, it kills the PCI bus driver, and then every
//!   driver of a disk controller, among them the one of the root's disk.
//!   Around each kill it writes and syncs part of a file on the root
//!   before the kill and the rest after it, and the file must read back
//!   as written.
//! - As `system.acpi-restart-probe`, in the ACPI restart run, it kills
//!   the ACPI server once the real-time clock's driver has published its
//!   service, which must still read the clock afterwards.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Filesystem, FilesystemAsyncClient, Node, NodeAsyncClient, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::time::{Rtc, RtcAsyncClient};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the `log_manage` boot handle among the `USER` startup
/// handles.
const LOG_ID: u32 = 2;
const FILESYSTEM: &str = "device.filesystem";
const RTC: &str = "device.rtc";
const BUS_DRIVER: &str = "system.driver.pci-bus";
/// The drivers of the disk controllers that the root's disk can be
/// behind. The probe kills them all, as it cannot tell which process
/// serves the root's disk.
const DISK_DRIVERS: [&str; 3] = [
    "system.driver.virtio-blk",
    "system.driver.nvme",
    "system.driver.ahci",
];
const ACPI_DRIVER: &str = "system.driver.acpi";
/// The app ID that kills the ACPI server.
const ACPI_PROBE: &str = "system.acpi-restart-probe";
const DRIVER_PREFIX: &str = "system.driver.";
const KILL: u32 = 9;
const MS: i64 = 1_000_000;
/// How long the root may take to be published, the processor emulated,
/// and how long the bus driver may take to run again.
const ROOT_LIMIT: i64 = 240_000 * MS;
const RESTART_LIMIT: i64 = 30_000 * MS;
/// How long the device manager has, once the bus driver runs again, to
/// reconnect the drivers below it: the bus driver adds its functions at
/// once.
const SETTLE: i64 = 3_000 * MS;
/// Before the kill, how long the drivers must run unchanged, and how long
/// after that the probe waits for the last of them to complete `Start`:
/// a driver whose bus channel closes during `Start` fails it, which the
/// run is not about. The AHCI driver takes about a second to look at the
/// ports of an empty controller.
const STABLE: i64 = 3_000 * MS;
const STARTING: i64 = 5_000 * MS;
/// The file written across each kill, in pieces of `PIECE` bytes:
/// `BEFORE` before the kill and `AFTER` after it, each synced.
const FILE: &str = "bus-restart";
const PIECE: usize = 16384;
const BEFORE: u32 = 4;
const AFTER: u32 = 8;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// Logs what failed, and gives `None`.
fn failed<T>(what: &str) -> Option<T> {
    mikros_rt::log!(Error, "restart-probe: FAILED: {what}");
    None
}

/// The bytes of piece `i` of the file.
fn piece(i: u32) -> Vec<u8> {
    (0..PIECE)
        .map(|at| (at as u32 ^ i.wrapping_mul(0x9E37)) as u8)
        .collect()
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let log = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID);
    if let Some(log) = log {
        mikros_rt::log::set_handle(log.handle.raw());
    }
    let header = managed.header;
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
    let passed = match header.app_id == ACPI_PROBE {
        true => executor.block_on(acpi(&directory, &launcher, &reactor)),
        false => executor.block_on(bus(&directory, &launcher, &reactor)),
    };
    if passed.is_some() {
        mikros_rt::log!(Info, "restart-probe: PASSED");
    }
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}

/// The drivers that run, as their app IDs and process IDs.
async fn drivers(launcher: &LauncherAsyncClient) -> Option<BTreeSet<(String, u32)>> {
    let Ok(listed) = launcher.list().await else {
        return failed("listing the processes");
    };
    let mut drivers = BTreeSet::new();
    for pid in listed.pids {
        if let Ok(Ok(info)) = launcher.get_info(pid).await
            && info.info.app_id.starts_with(DRIVER_PREFIX)
        {
            drivers.insert((info.info.app_id, pid));
        }
    }
    Some(drivers)
}

/// The root of the root file system, read and write, once it is
/// published.
async fn root(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<NodeAsyncClient> {
    let limit = now() + ROOT_LIMIT;
    loop {
        let listed = directory.list(FILESYSTEM.into()).await;
        if matches!(&listed, Ok(Ok(l)) if l.instances.iter().any(|i| i == "root")) {
            break;
        }
        if now() > limit {
            return failed("waiting for the root file system");
        }
        let _ = reactor.sleep_until(now() + 50 * MS).await;
    }
    let (client, server) = endpoints::<Filesystem>().ok()?;
    let connected = directory
        .connect(FILESYSTEM.into(), Some("root".into()), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the root file system");
    }
    let fs = FilesystemAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
    let (node, node_server) = endpoints::<Node>().ok()?;
    let rights = Rights(Rights::READ.0 | Rights::WRITE.0);
    if !matches!(fs.get_root(node_server, rights).await, Ok(Ok(_))) {
        return failed("opening the root");
    }
    Some(NodeAsyncClient::new(node, reactor.clone()))
}

/// Waits until the drivers have run unchanged for `STABLE` and then
/// `STARTING` more, kills every process of the apps `apps`, at least
/// one, and gives the drivers that ran then and the processes killed.
async fn kill(
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
    apps: &[&str],
) -> Option<(BTreeSet<(String, u32)>, Vec<(String, u32)>)> {
    let mut before = drivers(launcher).await?;
    let mut since = now();
    while now() < since + STABLE {
        let _ = reactor.sleep_until(now() + 100 * MS).await;
        let running = drivers(launcher).await?;
        if running != before {
            (before, since) = (running, now());
        }
    }
    let _ = reactor.sleep_until(now() + STARTING).await;
    let before = drivers(launcher).await?;
    let found: Vec<(String, u32)> = before
        .iter()
        .filter(|(a, _)| apps.contains(&a.as_str()))
        .cloned()
        .collect();
    if found.is_empty() {
        return failed("finding a driver to kill");
    }
    for (app, pid) in &found {
        if !matches!(launcher.kill(*pid, KILL).await, Ok(Ok(_))) {
            return failed("killing the driver");
        }
        mikros_rt::log!(Info, "restart-probe: killed {app}, pid {pid}");
    }
    Some((before, found))
}

/// Waits until each app of `killed` runs again as many processes as
/// were killed of it, none of them killed, and then `SETTLE` longer, and
/// checks that every driver of `before` of another app still runs as the
/// process it was.
async fn restarted(
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
    killed: &[(String, u32)],
    before: &BTreeSet<(String, u32)>,
) -> Option<()> {
    let limit = now() + RESTART_LIMIT;
    let is_killed = |app: &str| killed.iter().any(|(a, _)| a == app);
    loop {
        let running = drivers(launcher).await?;
        let again = |app: &String| {
            let new = running
                .iter()
                .filter(|&(a, pid)| a == app && !killed.contains(&(a.clone(), *pid)));
            new.count() >= killed.iter().filter(|(a, _)| a == app).count()
        };
        if killed.iter().all(|(app, _)| again(app)) {
            break;
        }
        if now() > limit {
            return failed("waiting for the driver to run again");
        }
        let _ = reactor.sleep_until(now() + 100 * MS).await;
    }
    let _ = reactor.sleep_until(now() + SETTLE).await;
    let after = drivers(launcher).await?;
    for (other, pid) in before.iter().filter(|(a, _)| !is_killed(a)) {
        if !after.contains(&(other.clone(), *pid)) {
            mikros_rt::log!(Error, "restart-probe: {other}, pid {pid}, no longer runs");
            return failed("keeping the other drivers");
        }
    }
    Some(())
}

/// Kills the ACPI server once the clock's service is published, and
/// reads the clock once the server runs again.
async fn acpi(
    directory: &DirectoryAsyncClient,
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
) -> Option<()> {
    let rtc = rtc(directory, reactor).await?;
    let (before, killed) = kill(launcher, reactor, &[ACPI_DRIVER]).await?;
    restarted(launcher, reactor, &killed, &before).await?;
    match rtc.get().await {
        Ok(Ok(_)) => Some(()),
        _ => failed("reading the clock after the ACPI server restarted"),
    }
}

/// The clock's service, once it is published.
async fn rtc(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<RtcAsyncClient> {
    let limit = now() + ROOT_LIMIT;
    let instance = loop {
        if let Ok(Ok(listed)) = directory.list(RTC.into()).await
            && let Some(instance) = listed.instances.into_iter().next()
        {
            break instance;
        }
        if now() > limit {
            return failed("waiting for the clock's service");
        }
        let _ = reactor.sleep_until(now() + 50 * MS).await;
    };
    let (client, server) = endpoints::<Rtc>().ok()?;
    let connected = directory
        .connect(RTC.into(), Some(instance), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the clock's service");
    }
    Some(RtcAsyncClient::new(client, reactor.clone()))
}

/// Kills the PCI bus driver below the root file system, and then the
/// drivers of the disk controllers, each time between writes to a file
/// there, which must read back as written after each restart.
async fn bus(
    directory: &DirectoryAsyncClient,
    launcher: &LauncherAsyncClient,
    reactor: &Reactor,
) -> Option<()> {
    let root = root(directory, reactor).await?;
    let (file, server) = endpoints::<Node>().ok()?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let rights = Rights(Rights::READ.0 | Rights::WRITE.0);
    if !matches!(
        root.open(FILE.into(), flags, 0o644, rights, server).await,
        Ok(Ok(_))
    ) {
        return failed("creating the file");
    }
    let file = NodeAsyncClient::new(file, reactor.clone());
    let write = async |i: u32| -> Option<()> {
        let at = u64::from(i) * PIECE as u64;
        let written = file.write(at, piece(i)).await;
        if !matches!(written, Ok(Ok(w)) if w.written as usize == PIECE) {
            return failed("writing");
        }
        if !matches!(file.sync().await, Ok(Ok(_))) {
            return failed("syncing");
        }
        Some(())
    };
    let read_back = async |pieces: u32| -> Option<()> {
        for i in 0..pieces {
            let at = u64::from(i) * PIECE as u64;
            let mut read = Vec::new();
            while read.len() < PIECE {
                let want = (PIECE - read.len()) as u32;
                let data = match file.read(at + read.len() as u64, want).await {
                    Ok(Ok(r)) => r.data,
                    _ => return failed("reading the file back"),
                };
                if data.is_empty() {
                    return failed("reading the whole file back");
                }
                read.extend_from_slice(&data);
            }
            if read != piece(i) {
                return failed("finding what was written");
            }
        }
        Some(())
    };

    let mut pieces = 0;
    for apps in [&[BUS_DRIVER][..], &DISK_DRIVERS[..]] {
        for i in pieces..pieces + BEFORE {
            write(i).await?;
        }
        let (before, killed) = kill(launcher, reactor, apps).await?;
        for i in pieces + BEFORE..pieces + BEFORE + AFTER {
            write(i).await?;
        }
        restarted(launcher, reactor, &killed, &before).await?;
        pieces += BEFORE + AFTER;
        read_back(pieces).await?;
    }
    if !matches!(root.unlink(FILE.into(), false).await, Ok(Ok(_))) {
        return failed("removing the file");
    }
    Some(())
}
