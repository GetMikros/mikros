//! The `cache` property of disks and partitions, which their file system
//! servers follow: from the storage profiles in the root file system
//! when there is a matching one, and from defaults otherwise.

use crate::devices::Device;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_manifest::profiles::{Cache, Profile, choose_cache, parse_profiles};
use mikros_rt::exec::Reactor;
use mikros_rt::files;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{FilesystemAsyncClient, Node, NodeAsyncClient, Rights};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

/// The hardware IDs of disks, partitions, and the Mikros root partition.
const DISK: &str = "storage:disk";
const PARTITION: &str = "storage:partition";
const ROOT_TYPE: &str = "gpt:fa730af6-8ba3-42d9-b155-466909d08135";
/// The properties that choose a device's policy, and the one it gets.
const CACHE: &str = "cache";
const REMOVABLE: &str = "removable";
const SERIAL: &str = "serial";
const PARTITION_GUID: &str = "partition_guid";
/// The protocol of file systems' published services.
const FILESYSTEM: &str = "device.filesystem";
/// The profiles' path in the root file system, and their largest size.
const PROFILES: &str = "etc/storage/profiles.yaml";
const PROFILES_MAX: usize = 64 << 10;

/// Which partition is the root, from the command line.
#[derive(Debug, PartialEq, Eq)]
enum Root {
    /// `root=bootfs`: there is no root partition.
    BootFs,
    /// `root=PARTUUID=<guid>`.
    Guid(String),
    /// Neither: the partition of the Mikros root type.
    OfRootType,
}

/// The root file system, once its `filesystem` service is published and
/// connected, from which the profiles are read.
pub struct Profiles {
    root: Root,
    connecting: Cell<bool>,
    fs: RefCell<Option<Rc<NodeAsyncClient>>>,
}

fn property<'a>(properties: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let (_, value) = properties.iter().find(|(k, _)| k == key)?;
    Some(value)
}

impl Profiles {
    /// The profiles of a system started with `command_line`.
    pub fn new(command_line: &str) -> Profiles {
        let mut root = Root::OfRootType;
        for word in command_line.split_ascii_whitespace() {
            if word == "root=bootfs" {
                root = Root::BootFs;
            } else if let Some(guid) = word.strip_prefix("root=PARTUUID=") {
                root = Root::Guid(guid.into());
            }
        }
        Profiles {
            root,
            connecting: Cell::new(false),
            fs: RefCell::new(None),
        }
    }

    /// Whether the `filesystem` service that the driver of `device`
    /// published is the root file system's, and none was taken before.
    pub fn is_new_root(&self, device: &Device) -> bool {
        if self.connecting.get() {
            return false;
        }
        match &self.root {
            Root::BootFs => false,
            Root::Guid(guid) => property(&device.properties, PARTITION_GUID)
                .is_some_and(|ours| ours.eq_ignore_ascii_case(guid)),
            Root::OfRootType => device.hardware_ids.iter().any(|id| id == ROOT_TYPE),
        }
    }

    /// Connects to instance `instance` of the published file systems as
    /// the root file system, through `directory`, and keeps its root
    /// directory to read the profiles from. A failure is logged, and
    /// leaves the defaults.
    pub async fn connect(
        &self,
        directory: &DirectoryAsyncClient,
        reactor: &Reactor,
        instance: String,
    ) {
        self.connecting.set(true);
        let Ok((client, server)) = endpoints::<mikros_rt::interfaces::mikros::fs::Filesystem>()
        else {
            return;
        };
        let connected = directory
            .connect(
                FILESYSTEM.into(),
                Some(instance.clone()),
                server.into_handle(),
            )
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            mikros_rt::log!(
                Error,
                "device-manager: connecting to the root file system {instance} failed"
            );
            return;
        }
        let fs = FilesystemAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
        let Ok((node, node_server)) = endpoints::<Node>() else {
            return;
        };
        match fs.get_root(node_server, Rights::READ).await {
            Ok(Ok(_)) => {
                *self.fs.borrow_mut() = Some(Rc::new(NodeAsyncClient::new(node, reactor.clone())));
                mikros_rt::log!(
                    Info,
                    "device-manager: reading storage profiles from the root file system {instance}"
                );
            }
            failed => mikros_rt::log!(
                Error,
                "device-manager: opening the root of file system {instance} failed: {:?}",
                failed.map(|r| r.err())
            ),
        }
    }

    /// The profiles, read again from the root file system, or none
    /// before it is connected, or when the file is missing or invalid.
    /// Keys that profiles do not use are ignored and logged.
    pub async fn load(&self, reactor: &Reactor) -> Option<Vec<Profile>> {
        let fs = self.fs.borrow().clone()?;
        let bytes = files::read(&fs, reactor, PROFILES, PROFILES_MAX)
            .await
            .ok()?;
        let Some(file) = parse_profiles(&bytes) else {
            mikros_rt::log!(
                Warning,
                "device-manager: {PROFILES} is invalid, so the defaults apply"
            );
            return None;
        };
        for key in &file.unknown {
            mikros_rt::log!(Warning, "device-manager: {PROFILES}: unknown key {key}");
        }
        Some(file.profiles)
    }
}

/// Whether a device with the hardware IDs `ids` is a disk or partition.
pub fn applies(ids: &[String]) -> bool {
    ids.iter().any(|id| id == DISK || id == PARTITION)
}

/// Gives a device with the hardware IDs `ids` and the properties
/// `properties`, added under `parent`, its `cache` property, replacing
/// any that its bus driver gave, when it is a disk or a partition: the
/// policy of the first of `profiles` that matches its `serial` or
/// `partition_guid`, and otherwise
/// - for a disk, `quick-disconnect` when its `removable` property is
///   `true`, and `write-back` otherwise;
/// - for a partition, its disk's value, and `write-back` when its parent
///   has none.
///
/// Other devices keep their properties as they are.
pub fn set(
    ids: &[String],
    properties: &mut Vec<(String, String)>,
    parent: Option<&Device>,
    profiles: &[Profile],
) {
    if !applies(ids) {
        return;
    }
    let chosen = choose_cache(
        profiles,
        property(properties, SERIAL),
        property(properties, PARTITION_GUID),
    );
    let value = match chosen {
        Some(cache) => cache.name(),
        None if ids.iter().any(|id| id == DISK) => match property(properties, REMOVABLE) {
            Some("true") => Cache::QuickDisconnect.name(),
            _ => Cache::WriteBack.name(),
        },
        None => parent
            .and_then(|p| property(&p.properties, CACHE))
            .unwrap_or(Cache::WriteBack.name()),
    };
    let value = String::from(value);
    properties.retain(|(k, _)| k != CACHE);
    properties.push((CACHE.into(), value));
}
