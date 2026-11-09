//! Publishing drivers' services in the service directory, as instances
//! of `device.<class>` that the device manager registers with the
//! service manager's `Registrar`.

use crate::start::duplicate;
use crate::supervise::Manager;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::__midl::ClientEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::Provider;

/// The classes of services that drivers may publish, and the permission
/// that connecting to each one's instances needs.
const CLASSES: [(&str, &str); 8] = [
    ("gpu", "gpu.render"),
    ("input", "input.seat"),
    ("acpi", "device.registry"),
    ("storage", "storage.raw"),
    ("filesystem", "filesystem.mount"),
    ("rtc", "time.set"),
    ("hid", "input.hid_raw"),
    ("subvolumes", "filesystem.manage"),
];
/// The class of file systems, and that of the management of a file
/// system's subvolumes, whose instance is named after the file system's.
const FILESYSTEM: &str = "filesystem";
const SUBVOLUMES: &str = "subvolumes";
/// The property of a partition's GUID, and the instance name of the root
/// partition's file system.
const PARTITION_GUID: &str = "partition_guid";
const ROOT: &str = "root";
/// The instance name of the file system of the boot's EFI system partition.
const ESP: &str = "esp";
/// The property that keeps a partition's file system from being a volume.
const NO_AUTOMOUNT: &str = "no_automount";

/// The service IDs given out during this boot, by device, class, and
/// how many services of that class the device's driver published before
/// since it was started, and the ID that the next new one gets.
pub struct ServiceIds {
    ids: RefCell<BTreeMap<(u64, String, usize), u64>>,
    next: Cell<u64>,
}

impl ServiceIds {
    pub fn new() -> ServiceIds {
        ServiceIds {
            ids: RefCell::new(BTreeMap::new()),
            next: Cell::new(1),
        }
    }

    /// The ID of the `nth` service of `class` of device `device`.
    fn get(&self, device: u64, class: &str, nth: usize) -> u64 {
        let mut ids = self.ids.borrow_mut();
        *ids.entry((device, class.into(), nth)).or_insert_with(|| {
            let id = self.next.get();
            self.next.set(id + 1);
            id
        })
    }
}

/// The services that one start of a driver published: how many of each
/// class, the registered instances by protocol and name, the ID of the
/// last `filesystem` service, and whether the driver has ended, after
/// which they are unregistered.
#[derive(Default)]
pub struct Published {
    counts: RefCell<BTreeMap<String, usize>>,
    instances: RefCell<Vec<(String, String)>>,
    filesystem: Cell<Option<u64>>,
    ended: Cell<bool>,
}

/// Registers `provider` as the next service of `class` of the driver of
/// device `caller`, which `published` keeps, and returns its ID. The
/// instance is named after the ID, but a `subvolumes` service's after the
/// ID of the last `filesystem` service of the same start of the driver.
/// A `filesystem` service of a partition is also registered, through
/// duplicates of `provider`, as `partuuid-<guid>`, the GUID in lowercase,
/// as `root` when the partition is the root partition, as `esp` when it
/// is the boot's EFI system partition, and otherwise as `volume-<guid>`
/// unless its `no_automount` property is `true`. Fails with
/// `INVALID_ARGS` for a class that drivers may not publish, and with
/// `BAD_STATE` for a `subvolumes` service before any `filesystem`
/// service.
pub async fn publish(
    m: &Manager,
    published: &Published,
    caller: u64,
    class: String,
    provider: ClientEnd<Provider>,
) -> Result<u64, Status> {
    let permission = CLASSES.iter().find(|(c, _)| *c == class);
    let (_, permission) = permission.ok_or(Status::InvalidArgs)?;
    let registrar = m.registrar.as_ref().ok_or(Status::BadState)?;
    let named = match class.as_str() {
        SUBVOLUMES => Some(published.filesystem.get().ok_or(Status::BadState)?),
        _ => None,
    };
    let nth = {
        let mut counts = published.counts.borrow_mut();
        let count = counts.entry(class.clone()).or_default();
        *count += 1;
        *count - 1
    };
    let id = m.service_ids.get(caller, &class, nth);
    if class == FILESYSTEM {
        published.filesystem.set(Some(id));
    }
    let instance = format!("{}", named.unwrap_or(id));
    let protocol = format!("device.{class}");
    let permission = Some(String::from(*permission));
    let aliases = match class.as_str() {
        FILESYSTEM => aliases(m, caller),
        _ => Vec::new(),
    };
    let provider = provider.into_handle();
    let mut copies = Vec::new();
    for _ in &aliases {
        copies.push(duplicate(&provider).map_err(|_| Status::NoResources)?);
    }
    let registered = registrar
        .register(
            protocol.clone(),
            instance.clone(),
            permission.clone(),
            ClientEnd::new(provider),
        )
        .await;
    match registered {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    }
    keep(m, published, caller, protocol.clone(), instance).await;
    for (alias, copy) in aliases.into_iter().zip(copies) {
        let registered = registrar
            .register(
                protocol.clone(),
                alias.clone(),
                permission.clone(),
                ClientEnd::new(copy),
            )
            .await;
        match registered {
            Ok(Ok(_)) => keep(m, published, caller, protocol.clone(), alias).await,
            failed => mikros_rt::log!(
                Error,
                "device-manager: publishing {protocol} {alias} for device {caller} failed: {:?}",
                failed.map(|r| r.err())
            ),
        }
    }
    Ok(id)
}

/// The other instance names of a `filesystem` service of the driver of
/// device `caller`: `partuuid-<guid>` for a partition, `root` as well
/// for the root partition, `esp` for the boot's EFI system partition,
/// and `volume-<guid>` for any other partition whose `no_automount`
/// property is not `true`.
fn aliases(m: &Manager, caller: u64) -> Vec<String> {
    let devices = m.devices.borrow();
    let Some(device) = devices.get(caller) else {
        return Vec::new();
    };
    let guid = device.properties.iter().find(|(k, _)| k == PARTITION_GUID);
    let Some((_, guid)) = guid else {
        return Vec::new();
    };
    let guid = guid.to_ascii_lowercase();
    let mut aliases = alloc::vec![format!("partuuid-{guid}")];
    let root = m.profiles.claim_root(device);
    if root {
        aliases.push(String::from(ROOT));
    }
    let esp = m.profiles.is_esp(device, &devices);
    if esp {
        aliases.push(String::from(ESP));
    }
    let no_automount = device
        .properties
        .iter()
        .any(|(k, v)| k == NO_AUTOMOUNT && v == "true");
    if !root && !esp && !no_automount {
        aliases.push(format!("volume-{guid}"));
    }
    aliases
}

/// Logs that instance `instance` of `protocol` is published for device
/// `caller`, and keeps it in `published` to unregister when the driver
/// ends, or unregisters it at once when it already did.
async fn keep(m: &Manager, published: &Published, caller: u64, protocol: String, instance: String) {
    mikros_rt::log!(
        Info,
        "device-manager: published {protocol} {instance} for device {caller}"
    );
    if published.ended.get() {
        unregister(m, &protocol, &instance).await;
    } else {
        published.instances.borrow_mut().push((protocol, instance));
    }
}

/// Unregisters the services in `published`, once their driver ended,
/// and those that are registered later.
pub async fn unregister_all(m: &Manager, published: &Published) {
    published.ended.set(true);
    let instances = core::mem::take(&mut *published.instances.borrow_mut());
    for (protocol, instance) in instances {
        unregister(m, &protocol, &instance).await;
    }
}

/// Unregisters the instance `instance` of `protocol`. One that the
/// service manager already removed, as its provider closed, counts as
/// unregistered.
async fn unregister(m: &Manager, protocol: &str, instance: &str) {
    let Some(registrar) = &m.registrar else {
        return;
    };
    match registrar.unregister(protocol.into(), instance.into()).await {
        Ok(Ok(_)) | Ok(Err(Status::NotFound)) => {
            mikros_rt::log!(Info, "device-manager: unregistered {protocol} {instance}")
        }
        failed => mikros_rt::log!(
            Error,
            "device-manager: unregistering {protocol} {instance} failed: {:?}",
            failed.map(|r| r.err())
        ),
    }
}
