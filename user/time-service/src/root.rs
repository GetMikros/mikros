//! The time service's files on the root file system: once the
//! `device.filesystem` instance `root` is published, which `watch` sees,
//! the service reads there, once, the `rtc` key of `etc/time.yaml`, which
//! says whether the real-time clock keeps UTC or local time, and the
//! system time zone, which the link `etc/localtime` names in
//! `usr/share/zoneinfo/`; and it makes that link anew when the time zone
//! is set.

use crate::clock::{NANOS_PER_SECOND, Time};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, Node, NodeAsyncClient, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

pub const FILESYSTEM: &str = "device.filesystem";
pub const ROOT: &str = "root";
const TIME_YAML: &str = "etc/time.yaml";
const LOCALTIME: &str = "localtime";
const ZONEINFO: &str = "/usr/share/zoneinfo/";
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const CHUNK: u32 = 32768;

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn ok<T>(reply: Reply<T>) -> Option<T> {
    reply.ok()?.ok()
}

/// A time zone: its name and its rules, which UTC has none of.
pub struct Zone {
    pub name: String,
    pub rules: Option<mikros_tz::Zone>,
}

impl Zone {
    pub fn utc() -> Zone {
        Zone {
            name: crate::clock::UTC.into(),
            rules: None,
        }
    }

    /// The UTC time of the local time `local`, in seconds since 1970.
    pub fn utc_of_local(&self, local: i64) -> i64 {
        self.rules.as_ref().map_or(local, |z| z.utc_of_local(local))
    }

    /// The local time of the UTC time `utc`, in seconds since 1970.
    pub fn local_of_utc(&self, utc: i64) -> i64 {
        self.rules
            .as_ref()
            .map_or(utc, |z| utc + i64::from(z.offset_at(utc)))
    }
}

/// Connects to the root file system through `directory`, once it is
/// published, and reads the time service's files there into `time`.
pub async fn connected(time: Rc<Time>, directory: Rc<DirectoryAsyncClient>, reactor: Reactor) {
    let Some(root) = connect(&directory, &reactor).await else {
        return;
    };
    let rtc_local = rtc_local(&root, &reactor).await;
    let zone = match localtime(&root, &reactor).await {
        Some(zone) => zone,
        None => Zone::utc(),
    };
    time.root_available(Rc::new(root), rtc_local, zone);
}

/// The root directory of the root file system, read and write; None when
/// it cannot be reached.
async fn connect(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<NodeAsyncClient> {
    let (client, server) = endpoints::<Filesystem>().ok()?;
    let connected = directory
        .connect(FILESYSTEM.into(), Some(ROOT.into()), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        mikros_rt::log!(Error, "time: connecting to the root file system failed");
        return None;
    }
    let fs = FilesystemAsyncClient::new(client, reactor.clone());
    let (root, server) = endpoints::<Node>().ok()?;
    ok(fs.get_root(server, RW).await)?;
    Some(NodeAsyncClient::new(root, reactor.clone()))
}

/// Opens `path` below `dir` with `flags`.
async fn open(
    dir: &NodeAsyncClient,
    path: &str,
    flags: OpenFlags,
    reactor: &Reactor,
) -> Option<NodeAsyncClient> {
    let (node, server) = endpoints::<Node>().ok()?;
    ok(dir.open(path.into(), flags, 0o644, RW, server).await)?;
    Some(NodeAsyncClient::new(node, reactor.clone()))
}

/// The bytes of the file at `path` below `dir`.
async fn read(dir: &NodeAsyncClient, path: &str, reactor: &Reactor) -> Option<Vec<u8>> {
    let file = open(dir, path, OpenFlags(0), reactor).await?;
    let mut bytes = Vec::new();
    loop {
        let data = ok(file.read(bytes.len() as u64, CHUNK).await)?.data;
        if data.is_empty() {
            return Some(bytes);
        }
        bytes.extend_from_slice(&data);
    }
}

/// Whether `etc/time.yaml` says that the real-time clock keeps local
/// time: its key `rtc` is `local`. A missing file or key, or another
/// value, means UTC.
async fn rtc_local(root: &NodeAsyncClient, reactor: &Reactor) -> bool {
    let Some(bytes) = read(root, TIME_YAML, reactor).await else {
        return false;
    };
    let Ok(node) = mikros_yaml::parse(&bytes) else {
        mikros_rt::log!(
            Warning,
            "time: {TIME_YAML} is not valid YAML, so the clock keeps UTC"
        );
        return false;
    };
    let value = node.map().ok().and_then(|m| m.get("rtc")?.string().ok());
    value == Some("local")
}

/// The system time zone, which `etc/localtime` names, with its rules
/// from the zone's file; None when the link is missing or names no
/// zone that can be read.
async fn localtime(root: &NodeAsyncClient, reactor: &Reactor) -> Option<Zone> {
    let link = open(root, "etc/localtime", OpenFlags::NO_FOLLOW, reactor).await?;
    let target = ok(link.read_link().await)?.target;
    let at = target.rfind(&ZONEINFO[1..])?;
    let name = &target[at + ZONEINFO.len() - 1..];
    match load(root, name, reactor).await {
        Ok(zone) => Some(zone),
        Err(_) => {
            mikros_rt::log!(
                Warning,
                "time: etc/localtime names no zone that can be read: {target}"
            );
            None
        }
    }
}

/// Whether `name` can name a file of `usr/share/zoneinfo/`, as IANA names
/// such as `America/Argentina/Buenos_Aires` and `Etc/GMT+5` do: parts of
/// letters, digits and `_`, `-`, `+` and `.`, none of them `.` or `..`.
fn is_zone_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-+.".contains(&b))
        })
}

/// The zone `name` with its rules from `usr/share/zoneinfo/` below
/// `root`: `NOT_FOUND` for a name without a file there, and `IO` for a
/// file that is no zone.
async fn load(root: &NodeAsyncClient, name: &str, reactor: &Reactor) -> Result<Zone, Status> {
    if !is_zone_name(name) {
        return Err(Status::NotFound);
    }
    let path = format!("{}{name}", &ZONEINFO[1..]);
    let bytes = read(root, &path, reactor).await.ok_or(Status::NotFound)?;
    let rules = mikros_tz::Zone::parse(&bytes).map_err(|_| Status::Io)?;
    Ok(Zone {
        name: name.into(),
        rules: Some(rules),
    })
}

/// Makes `name` the system time zone below `root`: reads its rules and
/// makes `etc/localtime` a link to its file.
pub async fn set_zone(
    root: &NodeAsyncClient,
    name: &str,
    reactor: &Reactor,
) -> Result<Zone, Status> {
    let zone = load(root, name, reactor).await?;
    let etc = open(root, "etc", OpenFlags::DIRECTORY, reactor)
        .await
        .ok_or(Status::Io)?;
    // A missing link is fine; another failure shows when it is made.
    let _ = etc.unlink(LOCALTIME.into(), false).await;
    let target = format!("{ZONEINFO}{name}");
    ok(etc.create_symlink(LOCALTIME.into(), target).await).ok_or(Status::Io)?;
    Ok(zone)
}

/// The offset of UTC from the monotonic clock that makes UTC at monotonic
/// time 0 the local time `boot_time`, in seconds, of `zone`.
pub fn local_offset(zone: &Zone, boot_time: i64) -> Option<i64> {
    zone.utc_of_local(boot_time).checked_mul(NANOS_PER_SECOND)
}
