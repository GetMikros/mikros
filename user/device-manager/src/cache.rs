//! The `cache` property of disks and partitions, which their file system
//! servers follow: from the storage profiles in the root file system
//! when there is a matching one, and from defaults otherwise.

use crate::devices::{Device, Devices};
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

/// The hardware IDs of disks, partitions, the Mikros root partition, and
/// EFI system partitions.
const DISK: &str = "storage:disk";
const PARTITION: &str = "storage:partition";
const ROOT_TYPE: &str = "gpt:fa730af6-8ba3-42d9-b155-466909d08135";
const ESP_TYPE: &str = "gpt:c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
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

/// The root partition's device, once the first one that matches `root`
/// publishes a file system, and its file system once connected, from
/// which the profiles are read, and the GUID of the EFI system partition
/// that `esp=PARTUUID=` gives.
pub struct Profiles {
    root: Root,
    root_device: Cell<Option<u64>>,
    fs: RefCell<Option<Rc<NodeAsyncClient>>>,
    esp: Option<String>,
}

fn property<'a>(properties: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let (_, value) = properties.iter().find(|(k, _)| k == key)?;
    Some(value)
}

impl Profiles {
    /// The profiles of a system started with `command_line`.
    pub fn new(command_line: &str) -> Profiles {
        let mut root = Root::OfRootType;
        let mut esp = None;
        for word in command_line.split_ascii_whitespace() {
            if word == "root=bootfs" {
                root = Root::BootFs;
            } else if let Some(guid) = word.strip_prefix("root=PARTUUID=") {
                root = Root::Guid(guid.into());
            } else if let Some(guid) = word.strip_prefix("esp=PARTUUID=") {
                esp = Some(guid.into());
            }
        }
        Profiles {
            root,
            root_device: Cell::new(None),
            fs: RefCell::new(None),
            esp,
        }
    }

    /// Whether `device` matches the command line's root partition.
    fn matches_root(&self, device: &Device) -> bool {
        match &self.root {
            Root::BootFs => false,
            Root::Guid(guid) => property(&device.properties, PARTITION_GUID)
                .is_some_and(|ours| ours.eq_ignore_ascii_case(guid)),
            Root::OfRootType => device.hardware_ids.iter().any(|id| id == ROOT_TYPE),
        }
    }

    /// Whether `device`, whose driver published a `filesystem` service,
    /// is the root partition: the first device that publishes one and
    /// matches the command line becomes it, and stays it.
    pub fn claim_root(&self, device: &Device) -> bool {
        if let Some(root) = self.root_device.get() {
            return root == device.id;
        }
        let matches = self.matches_root(device);
        if matches {
            self.root_device.set(Some(device.id));
        }
        matches
    }

    /// Whether `device`, among `devices`, is the EFI system partition of
    /// the boot: the one whose GUID `esp=PARTUUID=` gives, or else, unless
    /// there is no root partition, the first EFI system partition on the
    /// disk that holds the root partition, the one already chosen or else
    /// one that matches the command line.
    pub fn is_esp(&self, device: &Device, devices: &Devices) -> bool {
        if let Some(guid) = &self.esp {
            return property(&device.properties, PARTITION_GUID)
                .is_some_and(|ours| ours.eq_ignore_ascii_case(guid));
        }
        if self.root == Root::BootFs || !device.hardware_ids.iter().any(|id| id == ESP_TYPE) {
            return false;
        }
        let Some(disk) = device.parent else {
            return false;
        };
        let on_disk = |d: &&Device| d.parent == Some(disk);
        let root_here = match self.root_device.get() {
            Some(root) => devices.get(root).is_some_and(|r| r.parent == Some(disk)),
            None => devices.iter().filter(on_disk).any(|d| self.matches_root(d)),
        };
        let first_esp = devices
            .iter()
            .filter(on_disk)
            .find(|d| d.hardware_ids.iter().any(|id| id == ESP_TYPE));
        root_here && first_esp.is_some_and(|d| d.id == device.id)
    }

    /// Whether device `id` is the root partition that `claim_root` chose.
    pub fn is_root(&self, id: u64) -> bool {
        self.root_device.get() == Some(id)
    }

    /// Connects to instance `instance` of the published file systems as
    /// the root file system, through `directory`, and keeps its root
    /// directory to read the profiles from, in place of an earlier one.
    /// A failure is logged, and leaves what was there.
    pub async fn connect(
        &self,
        directory: &DirectoryAsyncClient,
        reactor: &Reactor,
        instance: String,
    ) {
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
