//! Serving the `DeviceManager` channel of each driver while it runs.

use crate::devices::Device;
use crate::supervise::{self, Manager};
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_manifest::Need;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManager, DeviceManagerAddDeviceRequest, DeviceManagerAddDeviceResponse,
    DeviceManagerAsyncServer, DeviceManagerRemoveDeviceResponse, DeviceManagerRequest,
    DeviceManagerRequestResourceRequest, DeviceManagerRequestResourceResponse,
};

/// The resource kinds that `RequestResource` creates.
const KIND_MMIO: u32 = 1;
const KIND_IOPORT: u32 = 2;
const KIND_IRQ: u32 = 3;
const KIND_MSI: u32 = 6;
const KIND_BTI: u32 = 8;
/// The hardware ID of PCI host bridges, whose buses' requester IDs a
/// `BTI` resource may cover.
const HOST_BRIDGE: &str = "platform:pci-host-ecam";

/// Serves the `DeviceManager` channel `end` of the driver of device
/// `caller` until it closes. Each request runs as a task of its own, so
/// that one that waits does not hold up the others. `PublishService`
/// fails with `NOT_SUPPORTED`.
pub async fn serve(m: Rc<Manager>, caller: u64, end: ServerEnd<DeviceManager>) {
    use DeviceManagerRequest as R;
    let server = Rc::new(DeviceManagerAsyncServer::new(end, m.reactor.clone()));
    while let Ok(request) = server.next().await {
        let (m, server) = (m.clone(), server.clone());
        m.spawner.clone().spawn(async move {
            let s = server.server();
            let _ = match request {
                R::AddDevice(request, responder) => {
                    let added = add_device(&m, caller, request);
                    let reply = added.map(|(id, _)| DeviceManagerAddDeviceResponse { id });
                    let replied = responder.reply(s, reply);
                    // Its driver starts once the bus driver knows its ID,
                    // which it may need to serve `ConnectChild`.
                    if let Ok((id, true)) = added {
                        m.spawner.spawn(supervise::supervise(m.clone(), id));
                    }
                    replied
                }
                R::RemoveDevice(request, responder) => {
                    let removed = remove_device(&m, caller, request.id).await;
                    responder.reply(s, removed.map(|()| DeviceManagerRemoveDeviceResponse {}))
                }
                R::PublishService(_, responder) => responder.reply(s, Err(Status::NotSupported)),
                R::RequestResource(request, responder) => {
                    let resource = request_resource(&m, caller, &request);
                    let reply =
                        resource.map(|resource| DeviceManagerRequestResourceResponse { resource });
                    responder.reply(s, reply)
                }
            };
        });
    }
}

/// Adds the device of `request` as a child of device `caller`, and binds
/// a driver to it. Fails with `INVALID_ARGS` when a hardware ID does not
/// have the form of one. Returns its ID and whether a driver is bound.
fn add_device(
    m: &Manager,
    caller: u64,
    request: DeviceManagerAddDeviceRequest,
) -> Result<(u64, bool), Status> {
    let ids = &request.hardware_ids;
    if !ids.iter().all(|id| mikros_manifest::is_hardware_id(id)) {
        return Err(Status::InvalidArgs);
    }
    let properties: Vec<(alloc::string::String, alloc::string::String)> = request
        .properties
        .into_iter()
        .map(|p| (p.key, p.value))
        .collect();
    let mut devices = m.devices.borrow_mut();
    let id = devices.add(
        request.name,
        request.hardware_ids,
        properties,
        Some(caller),
        request.resources,
    );
    let bound = devices.bind_device(id, &m.drivers);
    Ok((id, bound))
}

/// Removes device `id` for the driver of device `caller`, which must
/// have added it: `NOT_FOUND` when there is no such device, and
/// `ACCESS_DENIED` when its parent is another device.
async fn remove_device(m: &Rc<Manager>, caller: u64, id: u64) -> Result<(), Status> {
    let parent = m.devices.borrow().get(id).map(|d| d.parent);
    match parent {
        None => Err(Status::NotFound),
        Some(parent) if parent != Some(caller) => Err(Status::AccessDenied),
        Some(_) => {
            supervise::remove(m.clone(), id).await;
            Ok(())
        }
    }
}

/// The resource that `request` asks for, for the driver of device
/// `caller`. Fails with `ACCESS_DENIED` unless the driver's manifest
/// needs `bus`, and with `INVALID_ARGS` for kinds other than `MMIO`,
/// `IOPORT`, `IRQ`, `MSI` and `BTI`. A `BTI` resource also needs `dma`,
/// and only covers requester IDs of a host bridge's buses.
fn request_resource(
    m: &Manager,
    caller: u64,
    request: &DeviceManagerRequestResourceRequest,
) -> Result<OwnedHandle, Status> {
    let devices = m.devices.borrow();
    let device = devices.get(caller).ok_or(Status::BadState)?;
    let bound = device.driver.as_ref().ok_or(Status::BadState)?;
    let driver = m.drivers.iter().find(|d| &d.name == bound);
    let driver = driver.ok_or(Status::BadState)?;
    if !driver.needs.contains(&Need::Bus) {
        return Err(Status::AccessDenied);
    }
    let (kind, base, size) = (request.kind, request.base, request.size);
    if ![KIND_MMIO, KIND_IOPORT, KIND_IRQ, KIND_MSI, KIND_BTI].contains(&kind) {
        return Err(Status::InvalidArgs);
    }
    if kind == KIND_BTI {
        let (first, end) = requester_ids(device).ok_or(Status::AccessDenied)?;
        let covered = base
            .checked_add(size)
            .is_some_and(|e| base >= first && e <= end);
        if !driver.needs.contains(&Need::Dma) || !covered {
            return Err(Status::AccessDenied);
        }
    }
    m.starter.request(kind, base, size, request.shared)
}

/// The requester IDs of the buses of `device`, from the first to the one
/// after the last, when it is a PCI host bridge: those of every device
/// and function of its buses from `bus_start` to `bus_end`, which start
/// at `(segment << 16) | (bus_start << 8)`.
fn requester_ids(device: &Device) -> Option<(u64, u64)> {
    if !device.hardware_ids.iter().any(|id| id == HOST_BRIDGE) {
        return None;
    }
    let property = |key: &str| {
        let (_, value) = device.properties.iter().find(|(k, _)| k == key)?;
        value.parse::<u64>().ok()
    };
    let segment = property("segment")?;
    let (start, end) = (property("bus_start")?, property("bus_end")?);
    let first = (segment << 16) | (start << 8);
    let count = end.checked_sub(start)?.checked_add(1)? << 8;
    Some((first, first + count))
}
