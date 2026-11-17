//! `device.volumes`: listing the volumes, the devices with a
//! `partition_guid` and an `fs:` hardware ID other than the root
//! partition, by that ID, and ejecting and mounting them
//! (MIKSPEC-0025.R65).
//!
//! A volume is mounted while its `volume-<id>` instance is registered.
//! Ejecting one unregisters that instance. On a fixed disk, it then stops
//! the driver of the volume's device, which stays without a driver. On a
//! removable disk, it removes the devices below the device whose driver
//! serves the disk, the USB interface of `usbstorage`, and stops that
//! driver too; each volume removed so is recorded, with that device, until
//! the device is removed or the volume comes back. Mounting starts the
//! stopped driver again, and has the volume's instance registered even
//! when it is hidden, until it is ejected or removed. Ejecting needs
//! `user.volumes.access` and mounting `admin.volumes.mount`, which the
//! device manager asks the broker for.

use crate::start::duplicate;
use crate::supervise::{self, EJECT_STOP_LIMIT, Manager};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    Volume, Volumes, VolumesAsyncServer, VolumesEjectResponse, VolumesListResponse,
    VolumesMountResponse, VolumesRequest,
};
use mikros_rt::interfaces::mikros::permissions::Decision;
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The service that the device manager provides.
pub const PROTOCOL: &str = "device.volumes";
/// The permissions of ejecting and mounting.
const EJECT_PERMISSION: &str = "user.volumes.access";
const MOUNT_PERMISSION: &str = "admin.volumes.mount";
/// The protocol of file systems, the prefix of a volume's instance, and
/// the permission that connecting to it needs.
const FILESYSTEM: &str = "device.filesystem";
const VOLUME: &str = "volume-";
const FILESYSTEM_PERMISSION: &str = "system.filesystems.mount";
/// The properties and hardware IDs that make a device a volume.
const PARTITION_GUID: &str = "partition_guid";
const NO_AUTOMOUNT: &str = "no_automount";
const REMOVABLE: &str = "removable";
const FS: &str = "fs:";
/// The most volumes one `List` reply holds.
const LIST_MAX: usize = 64;
/// How long `Mount` waits for a recorded volume to be added again.
const MOUNT_LIMIT: i64 = 10_000_000_000;
const POLL: i64 = 100_000_000;

/// A volume that an eject from a removable disk removed.
struct Record {
    id: String,
    hidden: bool,
    /// The device whose driver the eject stopped.
    device: u64,
}

/// The state of the volumes: which IDs have their instance registered,
/// which `Mount` asked to be registered, the `filesystem` provider that
/// each running file system driver published, the instances that
/// `Mount` registered for a running driver, and the records.
#[derive(Default)]
pub struct State {
    registered: RefCell<BTreeSet<String>>,
    forced: RefCell<BTreeSet<String>>,
    providers: RefCell<BTreeMap<u64, OwnedHandle>>,
    extra: RefCell<BTreeMap<u64, String>>,
    records: RefCell<Vec<Record>>,
}

impl State {
    /// Whether `Mount` asked for the volume `id` to be registered.
    pub fn forced(&self, id: &str) -> bool {
        self.forced.borrow().contains(id)
    }

    /// Notes that the instance `instance` of `protocol` is registered, or
    /// no longer.
    pub fn note(&self, protocol: &str, instance: &str, registered: bool) {
        let Some(id) = instance
            .strip_prefix(VOLUME)
            .filter(|_| protocol == FILESYSTEM)
        else {
            return;
        };
        match registered {
            true => self.registered.borrow_mut().insert(id.into()),
            false => self.registered.borrow_mut().remove(id),
        };
    }

    /// Keeps a copy of the `filesystem` provider that the driver of
    /// device `device` published.
    pub fn keep_provider(&self, device: u64, provider: OwnedHandle) {
        self.providers.borrow_mut().insert(device, provider);
    }

    /// Forgets the volume `id` of a device that is removed, unless an
    /// eject removed it and recorded it.
    pub fn removed(&self, id: Option<&str>) {
        if let Some(id) = id
            && !self.records.borrow().iter().any(|r| r.id == id)
        {
            self.forced.borrow_mut().remove(id);
        }
    }
}

/// Once the driver of device `device` ended: drops its provider, and
/// unregisters the instance that `Mount` registered for it.
pub async fn driver_ended(m: &Manager, device: u64) {
    m.volumes.providers.borrow_mut().remove(&device);
    let extra = m.volumes.extra.borrow_mut().remove(&device);
    if let Some(instance) = extra {
        unregister(m, &instance).await;
    }
}

fn property<'a>(device: &'a crate::devices::Device, key: &str) -> Option<&'a str> {
    let (_, value) = device.properties.iter().find(|(k, _)| k == key)?;
    Some(value)
}

/// The ID of device `device` when it is a volume.
fn volume_id(m: &Manager, device: &crate::devices::Device) -> Option<String> {
    let id = property(device, PARTITION_GUID)?;
    let fs = device.hardware_ids.iter().any(|h| h.starts_with(FS));
    let volume = fs && !device.removing && !m.profiles.is_root(device.id);
    volume.then(|| id.to_ascii_lowercase())
}

/// A volume of a device: its device, its disk's, and whether it is
/// hidden and on a removable disk.
struct Found {
    device: u64,
    disk: Option<u64>,
    hidden: bool,
    removable: bool,
}

/// The volume `id` among the devices.
fn find(m: &Manager, id: &str) -> Option<Found> {
    let devices = m.devices.borrow();
    let device = devices
        .iter()
        .find(|d| volume_id(m, d).as_deref() == Some(id))?;
    let disk = device.parent.and_then(|p| devices.get(p));
    let no_automount = property(device, NO_AUTOMOUNT) == Some("true");
    Some(Found {
        device: device.id,
        disk: disk.map(|d| d.id),
        hidden: no_automount || m.profiles.is_esp(device, &devices),
        removable: disk.is_some_and(|d| property(d, REMOVABLE) == Some("true")),
    })
}

/// Drops the records whose device is gone or whose volume came back.
fn prune(m: &Manager) {
    let devices = m.devices.borrow();
    let live: BTreeSet<String> = devices.iter().filter_map(|d| volume_id(m, d)).collect();
    m.volumes
        .records
        .borrow_mut()
        .retain(|r| devices.get(r.device).is_some_and(|d| !d.removing) && !live.contains(&r.id));
}

/// The volumes whose ID is after `after`, at most `LIST_MAX`, in order.
fn list(m: &Manager, after: &str) -> Vec<Volume> {
    prune(m);
    let mut all: BTreeMap<String, Volume> = BTreeMap::new();
    let ids: Vec<String> = m
        .devices
        .borrow()
        .iter()
        .filter_map(|d| volume_id(m, d))
        .collect();
    for id in ids {
        if let Some(found) = find(m, &id) {
            let mounted = m.volumes.registered.borrow().contains(&id);
            let volume = Volume {
                id: id.clone(),
                mounted,
                hidden: found.hidden,
                removable: found.removable,
            };
            all.insert(id, volume);
        }
    }
    for record in m.volumes.records.borrow().iter() {
        all.entry(record.id.clone()).or_insert_with(|| Volume {
            id: record.id.clone(),
            mounted: false,
            hidden: record.hidden,
            removable: true,
        });
    }
    all.into_iter()
        .filter(|(id, _)| id.as_str() > after)
        .take(LIST_MAX)
        .map(|(_, v)| v)
        .collect()
}

/// Whether the broker allows `client` `permission` when asked with
/// `Request`. Without a broker, or without an answer, it does not.
async fn allowed(m: &Manager, client: &ClientIdentity, permission: &str) -> bool {
    let Some(broker) = &m.starter.broker else {
        return false;
    };
    let client = client.clone();
    let reason = format!("{} wants {permission}", client.app_id);
    matches!(
        broker.request(client, permission.into(), reason).await,
        Ok(Ok(r)) if r.decision == Decision::Allowed
    )
}

/// Unregisters the instance `instance` of `device.filesystem`.
async fn unregister(m: &Manager, instance: &str) {
    if let Some(registrar) = &m.registrar {
        let _ = registrar
            .unregister(FILESYSTEM.into(), instance.into())
            .await;
    }
    m.volumes.note(FILESYSTEM, instance, false);
    mikros_rt::log!(Info, "device-manager: unregistered {FILESYSTEM} {instance}");
}

/// Stops the driver of device `id` for good, waiting up to
/// `EJECT_STOP_LIMIT` for `Stop`, and leaves the device without a driver.
async fn stop_driver(m: &Rc<Manager>, id: u64) {
    let (client, pid) = match m.devices.borrow_mut().get_mut(id) {
        Some(device) => {
            device.stopped = true;
            (device.client.clone(), device.pid)
        }
        None => return,
    };
    supervise::stop_within(m, client, pid, EJECT_STOP_LIMIT).await;
}

/// Starts the driver of device `id` again, when an eject stopped it.
fn start_driver(m: &Rc<Manager>, id: u64) {
    let stopped = match m.devices.borrow_mut().get_mut(id) {
        Some(device) => core::mem::take(&mut device.stopped),
        None => false,
    };
    if stopped {
        m.spawner.spawn(supervise::supervise(m.clone(), id));
    }
}

/// Ejects the volume `id` for `client`.
async fn eject(m: &Rc<Manager>, client: &ClientIdentity, id: &str) -> Result<(), Status> {
    prune(m);
    let found = find(m, id);
    let recorded = m.volumes.records.borrow().iter().any(|r| r.id == id);
    if found.is_none() && !recorded {
        return Err(Status::NotFound);
    }
    if !allowed(m, client, EJECT_PERMISSION).await {
        return Err(Status::AccessDenied);
    }
    let found = find(m, id).ok_or(Status::BadState)?;
    if !m.volumes.registered.borrow().contains(id) {
        return Err(Status::BadState);
    }
    m.volumes.forced.borrow_mut().remove(id);
    unregister(m, &format!("{VOLUME}{id}")).await;
    if !found.removable {
        stop_driver(m, found.device).await;
        mikros_rt::log!(Info, "device-manager: ejected volume {id}");
        return Ok(());
    }
    // The device whose driver serves the disk, and the volumes below it.
    let server = {
        let devices = m.devices.borrow();
        found
            .disk
            .and_then(|disk| devices.get(disk))
            .and_then(|disk| disk.parent)
    };
    let Some(server) = server else {
        stop_driver(m, found.device).await;
        return Ok(());
    };
    let below = descendants(m, server);
    for device in &below {
        let volume = m
            .devices
            .borrow()
            .get(*device)
            .and_then(|d| volume_id(m, d));
        if let Some(volume) = volume
            && let Some(found) = find(m, &volume)
        {
            m.volumes.records.borrow_mut().push(Record {
                id: volume,
                hidden: found.hidden,
                device: server,
            });
        }
    }
    let children = m.devices.borrow().children(server);
    for child in children {
        supervise::remove_within(m.clone(), child, EJECT_STOP_LIMIT).await;
    }
    stop_driver(m, server).await;
    mikros_rt::log!(
        Info,
        "device-manager: ejected volume {id} and the disk of device {server}"
    );
    Ok(())
}

/// The devices below device `id`, at any depth.
fn descendants(m: &Manager, id: u64) -> Vec<u64> {
    let devices = m.devices.borrow();
    let mut all = Vec::new();
    let mut next = devices.children(id);
    while let Some(device) = next.pop() {
        next.extend(devices.children(device));
        all.push(device);
    }
    all
}

/// Mounts the volume `id` for `client`.
async fn mount(m: &Rc<Manager>, client: &ClientIdentity, id: &str) -> Result<(), Status> {
    prune(m);
    let found = find(m, id);
    let record = m
        .volumes
        .records
        .borrow()
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.device);
    if found.is_none() && record.is_none() {
        return Err(Status::NotFound);
    }
    if m.volumes.registered.borrow().contains(id) {
        return Err(Status::BadState);
    }
    if !allowed(m, client, MOUNT_PERMISSION).await {
        return Err(Status::AccessDenied);
    }
    m.volumes.forced.borrow_mut().insert(id.into());
    if let Some(found) = found {
        let running = m
            .devices
            .borrow()
            .get(found.device)
            .is_some_and(|d| !d.stopped);
        match running {
            true => register(m, found.device, id).await?,
            false => start_driver(m, found.device),
        }
        mikros_rt::log!(Info, "device-manager: mounted volume {id}");
        return Ok(());
    }
    let device = record.ok_or(Status::NotFound)?;
    start_driver(m, device);
    let deadline = supervise::now().saturating_add(MOUNT_LIMIT);
    while find(m, id).is_none() {
        if supervise::now() >= deadline {
            return Err(Status::TimedOut);
        }
        let _ = m.reactor.sleep_until(supervise::now() + POLL).await;
    }
    prune(m);
    mikros_rt::log!(Info, "device-manager: mounted volume {id}");
    Ok(())
}

/// Registers the `volume-<id>` instance for the running driver of device
/// `device`, through a copy of the provider it published, which is
/// unregistered when the driver ends. Without one yet, the driver
/// registers it when it publishes its file system.
async fn register(m: &Manager, device: u64, id: &str) -> Result<(), Status> {
    let provider = match m.volumes.providers.borrow().get(&device) {
        Some(provider) => duplicate(provider).map_err(|_| Status::NoResources)?,
        None => return Ok(()),
    };
    let registrar = m.registrar.as_ref().ok_or(Status::BadState)?;
    let instance = format!("{VOLUME}{id}");
    let registered = registrar
        .register(
            FILESYSTEM.into(),
            instance.clone(),
            Some(FILESYSTEM_PERMISSION.into()),
            ClientEnd::new(provider),
        )
        .await;
    match registered {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    }
    mikros_rt::log!(
        Info,
        "device-manager: published {FILESYSTEM} {instance} for device {device}"
    );
    m.volumes.note(FILESYSTEM, &instance, true);
    m.volumes.extra.borrow_mut().insert(device, instance);
    Ok(())
}

/// Serves `Volumes` for `client` on `end` until it closes. Each `Eject`
/// and `Mount` runs as a task of its own, so that `List` is answered
/// while one waits.
pub async fn serve(m: Rc<Manager>, client: ClientIdentity, end: ServerEnd<Volumes>) {
    let server = Rc::new(VolumesAsyncServer::new(end, m.reactor.clone()));
    let client = Rc::new(client);
    while let Ok(request) = server.next().await {
        let (m, server, client) = (m.clone(), server.clone(), client.clone());
        match request {
            VolumesRequest::List(request, responder) => {
                let volumes = list(&m, &request.after);
                let _ = responder.reply(server.server(), Ok(VolumesListResponse { volumes }));
            }
            VolumesRequest::Eject(request, responder) => {
                m.spawner.clone().spawn(async move {
                    let done = eject(&m, &client, &request.id).await;
                    let reply = done.map(|()| VolumesEjectResponse {});
                    let _ = responder.reply(server.server(), reply);
                });
            }
            VolumesRequest::Mount(request, responder) => {
                m.spawner.clone().spawn(async move {
                    let done = mount(&m, &client, &request.id).await;
                    let reply = done.map(|()| VolumesMountResponse {});
                    let _ = responder.reply(server.server(), reply);
                });
            }
        }
    }
}
