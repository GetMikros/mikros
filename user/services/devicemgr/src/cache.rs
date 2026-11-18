//! The `cache` property of disks and partitions, which their file system
//! servers follow: from the storage profiles in the root file system
//! when there is a matching one, and from defaults otherwise. And the
//! root partition, which gets the properties `system` and `live` from
//! the command line. The root partition and the EFI system partition of
//! the boot are taken only from the boot disk: the disk at the location
//! that `boot_disk=` names, when the command line has it.

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
pub const CACHE: &str = "cache";
const REMOVABLE: &str = "removable";
const SERIAL: &str = "serial";
const PARTITION_GUID: &str = "partition_guid";
/// The property that each device below a PCI function on the way to a
/// disk has for the disk's location, and the hardware ID of the root
/// devices of PCI host bridges, whose children are PCI functions.
const LOCATION: &str = "location";
/// The property that only the boot's EFI system partition gets, which
/// parents may not give (MIKSPEC-0025.R74).
pub const BOOT_ESP: &str = "boot_esp";
const HOST_BRIDGE: &str = "platform:pci-host-ecam";
/// The root partition's properties: the booted system version and
/// whether the system is live.
const SYSTEM: &str = "system";
const LIVE: &str = "live";
/// The protocol of file systems' published services.
const FILESYSTEM: &str = "device.filesystem";
/// The profiles' path in the root file system, and their largest size.
const PROFILES: &str = "config/storage/profiles.yaml";
const PROFILES_MAX: usize = 64 << 10;

/// Which partition is the root, from the command line.
#[derive(Debug, PartialEq, Eq)]
enum Root {
    /// `root=PARTUUID=<guid>`.
    Guid(String),
    /// Neither: the partition of the Mikros root type on the boot disk.
    OfRootType,
}

/// The root partition's device, once the first one that matches `root`
/// is added or publishes a file system, and its file system once
/// connected, from which the profiles are read; the GUID of the EFI
/// system partition that `esp=PARTUUID=` gives; the boot disk's location
/// that `boot_disk=` gives; and the version that `system=` names and
/// whether `live` is a word of the command line.
pub struct Profiles {
    root: Root,
    root_device: Cell<Option<u64>>,
    fs: RefCell<Option<Rc<NodeAsyncClient>>>,
    esp: Option<String>,
    boot_location: Option<String>,
    system: Option<String>,
    live: bool,
}

/// Whether `device` has the partition GUID `guid`, in either case.
fn has_guid(device: &Device, guid: &str) -> bool {
    property(&device.properties, PARTITION_GUID).is_some_and(|ours| ours.eq_ignore_ascii_case(guid))
}

fn property<'a>(properties: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let (_, value) = properties.iter().find(|(k, _)| k == key)?;
    Some(value)
}

/// Whether `device`, among `devices`, is a PCI function: a child of a
/// host bridge's root device, which the device manager added itself.
fn is_pci_function(device: &Device, devices: &Devices) -> bool {
    let parent = device.parent.and_then(|p| devices.get(p));
    parent.is_some_and(|p| p.parent.is_none() && p.hardware_ids.iter().any(|id| id == HOST_BRIDGE))
}

/// The location of `device`, among `devices` (MIKSPEC-0025.R70): the name
/// of the PCI function that it is or is below, then the `location`
/// property of each device below that function on the way to it, each
/// after a `/`. None for a device below no PCI function.
fn location(device: &Device, devices: &Devices) -> Option<String> {
    let mut parts = Vec::new();
    let mut at = device;
    while !is_pci_function(at, devices) {
        if let Some(part) = property(&at.properties, LOCATION) {
            parts.push(part);
        }
        at = devices.get(at.parent?)?;
    }
    let mut location = at.name.clone();
    for part in parts.iter().rev() {
        location.push('/');
        location.push_str(part);
    }
    Some(location)
}

impl Profiles {
    /// The profiles of a system started with `command_line`.
    pub fn new(command_line: &str) -> Profiles {
        let mut root = Root::OfRootType;
        let mut esp = None;
        let mut boot_location = None;
        let mut system = None;
        let mut live = false;
        for word in command_line.split_ascii_whitespace() {
            if let Some(guid) = word.strip_prefix("root=PARTUUID=") {
                root = Root::Guid(guid.into());
            } else if let Some(guid) = word.strip_prefix("esp=PARTUUID=") {
                esp = Some(guid.into());
            } else if let Some(at) = word.strip_prefix("boot_disk=") {
                boot_location = Some(at.into());
            } else if let Some(version) = word.strip_prefix("system=") {
                system = Some(version.into());
            }
            live |= word == LIVE;
        }
        Profiles {
            root,
            root_device: Cell::new(None),
            fs: RefCell::new(None),
            esp,
            boot_location,
            system,
            live,
        }
    }

    /// Whether `device` is the partition the command line names as root,
    /// on whatever disk: the one with the GUID of `root=PARTUUID=`, or
    /// without it one of the Mikros root type.
    fn root_candidate(&self, device: &Device) -> bool {
        match &self.root {
            Root::Guid(guid) => has_guid(device, guid),
            Root::OfRootType => device.hardware_ids.iter().any(|id| id == ROOT_TYPE),
        }
    }

    /// Whether `device`, among `devices`, matches the command line's root
    /// partition (MIKSPEC-0045.R21): a candidate on the boot disk.
    fn matches_root(&self, device: &Device, devices: &Devices) -> bool {
        self.root_candidate(device) && self.on_boot_disk(device, devices)
    }

    /// Whether the partition `device`, among `devices`, lies on the boot
    /// disk.
    fn on_boot_disk(&self, device: &Device, devices: &Devices) -> bool {
        device
            .parent
            .is_some_and(|disk| self.boot_disk(disk, devices))
    }

    /// Whether device `disk`, among `devices`, is the boot disk: the disk
    /// at the location that `boot_disk=` gives; without that option, a
    /// disk holding the partition whose GUID `esp=PARTUUID=` gives, but
    /// not a removable one while a disk that is not removable holds it;
    /// without either option, any disk whose `removable` property is
    /// `false`.
    fn boot_disk(&self, disk: u64, devices: &Devices) -> bool {
        let Some(device) = devices.get(disk) else {
            return false;
        };
        let removable = |d: &Device| property(&d.properties, REMOVABLE) != Some("false");
        if let Some(at) = &self.boot_location {
            return location(device, devices).as_deref() == Some(at.as_str());
        }
        let Some(guid) = &self.esp else {
            return !removable(device);
        };
        let holds_esp = |disk: u64| {
            devices
                .iter()
                .any(|d| d.parent == Some(disk) && has_guid(d, guid))
        };
        let fixed_holds_esp = devices
            .iter()
            .filter(|d| d.hardware_ids.iter().any(|id| id == DISK) && !removable(d))
            .any(|d| holds_esp(d.id));
        holds_esp(disk) && !(removable(device) && fixed_holds_esp)
    }

    /// Whether `device`, among `devices`, whose driver published a
    /// `filesystem` service, is the root partition: the one that
    /// `give_root_properties` chose, or else the first device that
    /// publishes one and matches the command line, which stays it.
    pub fn claim_root(&self, device: &Device, devices: &Devices) -> bool {
        if let Some(root) = self.root_device.get() {
            return root == device.id;
        }
        let matches = self.matches_root(device, devices);
        if matches {
            self.root_device.set(Some(device.id));
        }
        matches
    }

    /// Makes device `id`, just added to `devices`, the root partition when
    /// none is yet and it matches the command line, and then gives it the
    /// property `system` with the version that `system=` names, when the
    /// command line has that option, and `live`, `true` or `false`, in
    /// place of any it had.
    pub fn give_root_properties(&self, id: u64, devices: &mut Devices) {
        if let Some(device) = devices.get(id) {
            self.log_elsewhere(device, devices);
        }
        if self.root_device.get().is_some() {
            return;
        }
        match devices.get(id) {
            Some(device) if self.matches_root(device, devices) => {}
            _ => return,
        }
        self.root_device.set(Some(id));
        let Some(device) = devices.get_mut(id) else {
            return;
        };
        let properties = &mut device.properties;
        properties.retain(|(k, _)| k != SYSTEM && k != LIVE);
        if let Some(version) = &self.system {
            properties.push((SYSTEM.into(), version.clone()));
        }
        let live = if self.live { "true" } else { "false" };
        properties.push((LIVE.into(), live.into()));
    }

    /// Logs `device`, among `devices`, when it would be the root or the
    /// EFI system partition but lies on a disk other than the one at the
    /// location of `boot_disk=`, which it is ignored for, such as a copy of
    /// the boot disk. Without that option the boot disk may not be known
    /// yet, and nothing is logged.
    fn log_elsewhere(&self, device: &Device, devices: &Devices) {
        if self.boot_location.is_none() || self.on_boot_disk(device, devices) {
            return;
        }
        let esp = self
            .esp
            .as_deref()
            .is_some_and(|guid| has_guid(device, guid));
        for (matches, what) in [(self.root_candidate(device), "root"), (esp, "EFI system")] {
            if matches {
                mikros_rt::log!(
                    Warning,
                    "device-manager: ignoring device {}, {}, which matches the {what} partition \
                     but is not on the boot disk",
                    device.id,
                    device.name
                );
            }
        }
    }

    /// Whether `device`, among `devices`, is the EFI system partition of
    /// the boot (MIKSPEC-0025.R71): the one whose GUID `esp=PARTUUID=`
    /// gives, on the boot disk when `boot_disk=` names it, or else the
    /// first EFI system partition on the disk that holds the root
    /// partition, the one already chosen or else one that matches the
    /// command line.
    pub fn is_esp(&self, device: &Device, devices: &Devices) -> bool {
        if let Some(guid) = &self.esp {
            let on_boot_disk = self.boot_location.is_none() || self.on_boot_disk(device, devices);
            return has_guid(device, guid) && on_boot_disk;
        }
        if !device.hardware_ids.iter().any(|id| id == ESP_TYPE) {
            return false;
        }
        let Some(disk) = device.parent else {
            return false;
        };
        let on_disk = |d: &&Device| d.parent == Some(disk);
        let root_here = match self.root_device.get() {
            Some(root) => devices.get(root).is_some_and(|r| r.parent == Some(disk)),
            None => devices
                .iter()
                .filter(on_disk)
                .any(|d| self.matches_root(d, devices)),
        };
        let first_esp = devices
            .iter()
            .filter(on_disk)
            .find(|d| d.hardware_ids.iter().any(|id| id == ESP_TYPE));
        root_here && first_esp.is_some_and(|d| d.id == device.id)
    }

    /// Whether the partition with `properties`, which the driver of disk
    /// `disk` adds, among `devices`, is the boot's EFI system partition:
    /// the one whose GUID `esp=PARTUUID=` gives, on the disk at the
    /// location of `boot_disk=` when the command line has it; none
    /// without `esp=` (MIKSPEC-0025.R74).
    pub fn boot_esp(&self, properties: &[(String, String)], disk: u64, devices: &Devices) -> bool {
        let Some(guid) = &self.esp else {
            return false;
        };
        let ours = property(properties, PARTITION_GUID);
        let named = ours.is_some_and(|ours| ours.eq_ignore_ascii_case(guid));
        let on_disk = self.boot_location.is_none() || self.boot_disk(disk, devices);
        named && on_disk
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
        // The boot's EFI system partition, which is written every boot.
        None if property(properties, BOOT_ESP) == Some("true") => Cache::QuickDisconnect.name(),
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
