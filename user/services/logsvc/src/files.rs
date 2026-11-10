//! The log's files: once the `device.filesystem` instance `root` is
//! published, every record goes to a new `var/log/system/boot-<n>.log`
//! there, and the oldest of those files go while they take more than
//! 256 MiB together; once the instance `esp` is published, the records
//! of the boot go to `EFI/Mikros/boot.log` there, which replaces the
//! previous boot's, within 10 seconds of coming. Missing directories are
//! made. When the service stops, both files get the last records and are
//! synced.

use crate::log::{Log, now};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, Node, NodeAsyncClient, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, DirectoryEvent};

/// The service the file systems are published as, and the instances of
/// the root and the EFI system partition.
const FILESYSTEM: &str = "device.filesystem";
const ROOT: &str = "root";
const ESP: &str = "esp";
/// Where the files go on each.
const SYSTEM_DIRECTORY: [&str; 3] = ["var", "log", "system"];
const ESP_DIRECTORY: [&str; 2] = ["EFI", "Mikros"];
const BOOT_LOG: &str = "boot.log";
/// The most bytes the files of the root take together.
const KEPT: u64 = 256 << 20;
/// The longest the boot log on the EFI system partition waits for the
/// records that came.
const ESP_EVERY: i64 = 10_000_000_000;
/// The most bytes one `Append` takes.
const APPEND_MAX: usize = 32768;
/// How often a pause looks whether the service stops.
const STEP: i64 = 100_000_000;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn ok<T>(reply: Reply<T>) -> Option<T> {
    reply.ok()?.ok()
}

/// Waits for `root` and `esp` to be published through `directory`, and
/// starts writing to each once it is.
pub async fn watch(
    log: Rc<Log>,
    directory: DirectoryAsyncClient,
    reactor: Reactor,
    spawner: Spawner,
) {
    if !matches!(directory.watch(FILESYSTEM.into()).await, Ok(Ok(_))) {
        mikros_rt::log!(Error, "log: watching {FILESYSTEM} failed");
        return;
    }
    let (mut root, mut esp) = (false, false);
    while !(root && esp) {
        // The service has no instances until a first one is registered.
        let listed = match directory.list(FILESYSTEM.into()).await {
            Ok(Ok(listed)) => listed.instances,
            Ok(Err(_)) => Vec::new(),
            Err(_) => return,
        };
        for (instance, started) in [(ROOT, &mut root), (ESP, &mut esp)] {
            if *started || !listed.iter().any(|i| i == instance) {
                continue;
            }
            *started = true;
            let Some(fs) = connect(&directory, &reactor, instance).await else {
                mikros_rt::log!(Error, "log: connecting to {FILESYSTEM} {instance} failed");
                continue;
            };
            match instance {
                ROOT => spawner.spawn(to_root(log.clone(), fs, reactor.clone())),
                _ => spawner.spawn(to_esp(log.clone(), fs, reactor.clone())),
            }
        }
        if root && esp {
            return;
        }
        loop {
            match directory.next_event().await {
                Ok(DirectoryEvent::OnInstanceAdded(added)) if added.protocol == FILESYSTEM => break,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }
}

/// The root directory of `instance` of the file system service, read
/// and write.
async fn connect(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    instance: &str,
) -> Option<NodeAsyncClient> {
    let (client, server) = endpoints::<Filesystem>().ok()?;
    let connected = directory
        .connect(
            FILESYSTEM.into(),
            Some(instance.into()),
            server.into_handle(),
        )
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return None;
    }
    let fs = FilesystemAsyncClient::new(client, reactor.clone());
    let (root, server) = endpoints::<Node>().ok()?;
    ok(fs.get_root(server, RW).await)?;
    Some(NodeAsyncClient::new(root, reactor.clone()))
}

/// Opens `name` in `dir` with `flags`, read and write.
async fn open(
    dir: &NodeAsyncClient,
    name: &str,
    flags: OpenFlags,
    reactor: &Reactor,
) -> Option<NodeAsyncClient> {
    let (node, server) = endpoints::<Node>().ok()?;
    ok(dir.open(name.into(), flags, 0o644, RW, server).await)?;
    Some(NodeAsyncClient::new(node, reactor.clone()))
}

/// The directory at `path` below `root`, made where it is missing.
async fn directory(
    root: NodeAsyncClient,
    path: &[&str],
    reactor: &Reactor,
) -> Option<NodeAsyncClient> {
    let mut dir = root;
    for name in path {
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
        dir = open(&dir, name, flags, reactor).await?;
    }
    Some(dir)
}

/// A file that records go to, and the latest record it holds.
struct Output {
    file: NodeAsyncClient,
    written: u64,
}

impl Output {
    /// Appends the records after the latest the file holds: the bytes
    /// appended, or None when an append failed.
    async fn append_new(&mut self, log: &Log) -> Option<u64> {
        let mut appended = 0;
        while let Some((lines, last)) = log.lines_after(self.written, APPEND_MAX) {
            let len = lines.len() as u64;
            ok(self.file.append(lines).await)?;
            self.written = last;
            appended += len;
        }
        Some(appended)
    }

    /// Writes the last records out as the service stops, and syncs.
    async fn finish(mut self, log: &Log) {
        let _ = self.append_new(log).await;
        let _ = self.file.sync().await;
        log.open_files.set(log.open_files.get() - 1);
    }
}

/// The number of a file of the root's log directory, `boot-<n>.log`.
fn number(name: &str) -> Option<u64> {
    let n = name.strip_prefix("boot-")?.strip_suffix(".log")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// The numbers and sizes of the files of `dir` named `boot-<n>.log`,
/// lowest first.
async fn boot_files(dir: &NodeAsyncClient) -> Option<Vec<(u64, u64)>> {
    let mut files = Vec::new();
    let mut cookie = 0;
    loop {
        let read = ok(dir.read_directory(cookie, true).await)?;
        if read.entries.is_empty() {
            files.sort_unstable();
            return Some(files);
        }
        for entry in read.entries {
            if let Some(n) = number(&entry.name) {
                files.push((n, entry.attributes.map_or(0, |a| a.size)));
            }
        }
        cookie = read.next;
    }
}

fn file_name(n: u64) -> String {
    format!("boot-{n}.log")
}

/// Appends every record, from the oldest kept, to a new file of
/// `var/log/system` on the root file system `root`, and then each that
/// comes, removing the oldest other files while they all take more than
/// `KEPT`.
async fn to_root(log: Rc<Log>, root: NodeAsyncClient, reactor: Reactor) {
    let Some(dir) = directory(root, &SYSTEM_DIRECTORY, &reactor).await else {
        mikros_rt::log!(Error, "log: making var/log/system failed");
        return;
    };
    let Some(mut older) = boot_files(&dir).await else {
        mikros_rt::log!(Error, "log: listing var/log/system failed");
        return;
    };
    let n = older.last().map_or(1, |&(n, _)| n + 1);
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let Some(file) = open(&dir, &file_name(n), flags, &reactor).await else {
        mikros_rt::log!(
            Error,
            "log: creating var/log/system/{} failed",
            file_name(n)
        );
        return;
    };
    log.open_files.set(log.open_files.get() + 1);
    let mut output = Output { file, written: 0 };
    let mut size = 0;
    loop {
        let Some(appended) = output.append_new(&log).await else {
            mikros_rt::log!(Error, "log: writing var/log/system/{} failed", file_name(n));
            log.open_files.set(log.open_files.get() - 1);
            return;
        };
        size += appended;
        let mut total = size + older.iter().map(|&(_, s)| s).sum::<u64>();
        while total > KEPT && !older.is_empty() {
            let (oldest, oldest_size) = older.remove(0);
            let _ = dir.unlink(file_name(oldest), false).await;
            total -= oldest_size;
        }
        if log.stopping() {
            output.finish(&log).await;
            return;
        }
        log.newer(output.written).await;
    }
}

/// Writes the records of the boot, from the oldest kept, to a new
/// `EFI/Mikros/boot.log` on the EFI system partition `esp`, and then
/// those that come, within `ESP_EVERY` of coming, syncing each time, so
/// that the file holds them should the boot fail.
async fn to_esp(log: Rc<Log>, esp: NodeAsyncClient, reactor: Reactor) {
    let Some(dir) = directory(esp, &ESP_DIRECTORY, &reactor).await else {
        mikros_rt::log!(Error, "log: making EFI/Mikros failed");
        return;
    };
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let Some(file) = open(&dir, BOOT_LOG, flags, &reactor).await else {
        mikros_rt::log!(Error, "log: creating EFI/Mikros/{BOOT_LOG} failed");
        return;
    };
    log.open_files.set(log.open_files.get() + 1);
    let mut output = Output { file, written: 0 };
    loop {
        if output.append_new(&log).await.is_none() {
            mikros_rt::log!(Error, "log: writing EFI/Mikros/{BOOT_LOG} failed");
            log.open_files.set(log.open_files.get() - 1);
            return;
        }
        let _ = output.file.sync().await;
        if log.stopping() {
            output.finish(&log).await;
            return;
        }
        log.newer(output.written).await;
        // Records that come meanwhile go with these.
        let deadline = now().saturating_add(ESP_EVERY);
        while !log.stopping() && now() < deadline {
            let _ = reactor
                .sleep_until(deadline.min(now().saturating_add(STEP)))
                .await;
        }
    }
}
