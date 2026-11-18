//! The `cache` property of disks and partitions, which their file system
//! servers follow: from the storage profiles in the root file system
//! when there is a matching one, and from defaults otherwise. And the
//! root partition, which gets the properties `system` and `live` from
//! the command line. The root partition and the EFI system partition of
//! the boot are taken only from the boot disk: the disk at the location
//! that `boot_disk=` names, when the command line has it.
//!
//! The root partition is decided once the boot disk's partition driver
//! has replied to `Start`, having added all its partitions, or, when the
//! boot disk is any disk that is not removable, once the partition
//! drivers of those that appeared within `BOOT_DISK_WAIT` have replied.
//! Until then the drivers of the candidates wait. When the boot disk has
//! not appeared by then, or the boot disk has no candidate or more than
//! one, the boot stops: no partition becomes the root, and one error
//! lists the candidates (MIKSPEC-0045.R21, MIKSPEC-0045.R22).

use crate::Manager;
use crate::devices::{Device, Devices};
use alloc::format;
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
/// How long after it started the device manager waits for the boot disk,
/// or, when the boot disk is any disk that is not removable, for such
/// disks to appear.
const BOOT_DISK_WAIT: i64 = 30_000_000_000;

/// Which partition is the root, from the command line.
#[derive(Debug, PartialEq, Eq)]
enum Root {
    /// `root=PARTUUID=<guid>`.
    Guid(String),
    /// Neither: the partition of the Mikros root type on the boot disk.
    OfRootType,
}

/// Where the choice of the root partition stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Pending,
    /// The root partition's device.
    Root(u64),
    /// The boot stopped, without a root partition.
    Stopped,
}

/// The choice of the root partition and its devices: the candidates
/// whose drivers wait for it, the disks whose partition drivers replied
/// to `Start`, and, once `BOOT_DISK_WAIT` has passed when the boot disk
/// is any disk that is not removable, those of such disks it still waits
/// for. Then the root partition's file system once connected, from which
/// the profiles are read; the GUID of the EFI system partition that
/// `esp=PARTUUID=` gives; the boot disk's location that `boot_disk=`
/// gives; and the version that `system=` names and whether `live` is a
/// word of the command line.
pub struct Profiles {
    root: Root,
    decision: Cell<Decision>,
    held: RefCell<Vec<u64>>,
    replied: RefCell<Vec<u64>>,
    due: RefCell<Option<Vec<u64>>>,
    fs: RefCell<Option<Rc<NodeAsyncClient>>>,
    esp: Option<String>,
    boot_location: Option<String>,
    system: Option<String>,
    live: bool,
}

/// Whether `device` is a disk.
fn is_disk(device: &Device) -> bool {
    device.hardware_ids.iter().any(|id| id == DISK)
}

/// Whether `device` may be removed, unless its `removable` property says
/// it is not.
fn removable(device: &Device) -> bool {
    property(&device.properties, REMOVABLE) != Some("false")
}

/// Starts the drivers of the devices `held`, which waited for the choice
/// of the root partition.
pub fn start_held(m: &Rc<Manager>, held: Vec<u64>) {
    for id in held {
        m.spawner.spawn(crate::supervise::supervise(m.clone(), id));
    }
}

/// Waits `BOOT_DISK_WAIT` from now, the start of the device manager, and
/// then lets the boot disk's deadline pass.
pub async fn boot_disk_deadline(m: Rc<Manager>) {
    let deadline = crate::supervise::now().saturating_add(BOOT_DISK_WAIT);
    let _ = m.reactor.sleep_until(deadline).await;
    let held = m.profiles.deadline(&mut m.devices.borrow_mut());
    start_held(&m, held);
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
            decision: Cell::new(Decision::Pending),
            held: RefCell::new(Vec::new()),
            replied: RefCell::new(Vec::new()),
            due: RefCell::new(None),
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

    /// Whether the driver of device `id`, just added to `devices`, which
    /// has one when `bound`, waits for the choice of the root partition:
    /// it is a candidate that may lie on the boot disk, and nothing is
    /// decided yet. A candidate that lies elsewhere is logged.
    pub fn added(&self, id: u64, devices: &Devices, bound: bool) -> bool {
        let Some(device) = devices.get(id) else {
            return false;
        };
        self.log_elsewhere(device, devices);
        let waits = bound
            && self.decision.get() == Decision::Pending
            && self.root_candidate(device)
            && (self.boot_location.is_none() || self.on_boot_disk(device, devices));
        if waits {
            self.held.borrow_mut().push(id);
        }
        waits
    }

    /// Notes that the driver of device `id`, among `devices`, replied to
    /// `Start`, or failed to start. For the partition driver of the boot
    /// disk this decides the root partition, and so it may for a disk
    /// that is not removable once `BOOT_DISK_WAIT` has passed, when the
    /// boot disk is any such disk. Returns the devices whose drivers then
    /// start.
    pub fn replied(&self, id: u64, devices: &mut Devices) -> Vec<u64> {
        if self.decision.get() != Decision::Pending {
            return Vec::new();
        }
        if !devices.get(id).is_some_and(is_disk) {
            return Vec::new();
        }
        self.replied.borrow_mut().push(id);
        let ready = if self.any_fixed_disk() {
            match self.due.borrow_mut().as_mut() {
                None => false,
                Some(due) => {
                    due.retain(|&d| d != id);
                    due.is_empty()
                }
            }
        } else {
            self.boot_disk(id, devices)
        };
        match ready {
            true => self.decide(devices, None),
            false => Vec::new(),
        }
    }

    /// Called `BOOT_DISK_WAIT` after the device manager started: stops
    /// the boot when the boot disk has not appeared, and, when the boot
    /// disk is any disk that is not removable, decides once the partition
    /// drivers of the ones that appeared have replied. Returns the
    /// devices whose drivers then start.
    pub fn deadline(&self, devices: &mut Devices) -> Vec<u64> {
        if self.decision.get() != Decision::Pending {
            return Vec::new();
        }
        if self.any_fixed_disk() {
            let replied = self.replied.borrow().clone();
            let due: Vec<u64> = devices
                .iter()
                .filter(|d| is_disk(d) && !removable(d) && !replied.contains(&d.id))
                .map(|d| d.id)
                .collect();
            let ready = due.is_empty();
            *self.due.borrow_mut() = Some(due);
            return match ready {
                true => self.decide(devices, None),
                false => Vec::new(),
            };
        }
        // The boot disk is there: its partition driver has not replied.
        if devices
            .iter()
            .any(|d| is_disk(d) && self.boot_disk(d.id, devices))
        {
            return Vec::new();
        }
        self.decide(
            devices,
            Some("the boot disk did not appear within 30 seconds"),
        )
    }

    /// Whether the boot disk is any disk that is not removable: the
    /// command line has neither `boot_disk=` nor `esp=`.
    fn any_fixed_disk(&self) -> bool {
        self.boot_location.is_none() && self.esp.is_none()
    }

    /// Chooses the root partition among `devices`, the only candidate on
    /// the boot disk, unless `stopped` says why the boot stops; without
    /// one, or with more than one, it stops too. A stopped boot logs the
    /// candidates seen anywhere. Returns the devices whose drivers waited
    /// for the choice, which then start.
    fn decide(&self, devices: &mut Devices, stopped: Option<&str>) -> Vec<u64> {
        let on_boot_disk: Vec<u64> = devices
            .iter()
            .filter(|d| self.matches_root(d, devices))
            .map(|d| d.id)
            .collect();
        let why = match (stopped, on_boot_disk.as_slice()) {
            (Some(why), _) => why,
            (None, [root]) => {
                self.decision.set(Decision::Root(*root));
                self.give_root_properties(*root, devices);
                return core::mem::take(&mut *self.held.borrow_mut());
            }
            (None, []) => "the boot disk has no root partition",
            (None, _) => "the boot disk has more than one root partition",
        };
        self.decision.set(Decision::Stopped);
        let mut candidates = String::new();
        for d in devices.iter().filter(|d| self.root_candidate(d)) {
            if !candidates.is_empty() {
                candidates.push_str("; ");
            }
            let at = location(d, devices).unwrap_or_else(|| "no location".into());
            let guid = property(&d.properties, PARTITION_GUID).unwrap_or("no GUID");
            candidates.push_str(&format!("{} at {at}, {guid}", d.name));
        }
        if candidates.is_empty() {
            candidates.push_str("none");
        }
        mikros_rt::log!(
            Error,
            "device-manager: stopping the boot without a root partition: {why}; candidates: {candidates}"
        );
        core::mem::take(&mut *self.held.borrow_mut())
    }

    /// Gives device `id`, among `devices`, the root partition's
    /// properties: `system` with the version that `system=` names, when
    /// the command line has that option, and `live`, `true` or `false`,
    /// in place of any it had.
    fn give_root_properties(&self, id: u64, devices: &mut Devices) {
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
        let root_here = match self.decision.get() {
            Decision::Root(root) => devices.get(root).is_some_and(|r| r.parent == Some(disk)),
            Decision::Stopped => false,
            Decision::Pending => devices
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

    /// Whether device `id` is the root partition that was decided.
    pub fn is_root(&self, id: u64) -> bool {
        self.decision.get() == Decision::Root(id)
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
