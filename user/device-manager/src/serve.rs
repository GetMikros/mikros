//! Serving the `DeviceManager` channel of each driver while it runs.

use crate::supervise::{self, Manager};
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManager, DeviceManagerAddDeviceRequest, DeviceManagerAddDeviceResponse,
    DeviceManagerAsyncServer, DeviceManagerRemoveDeviceResponse, DeviceManagerRequest,
};

/// Serves the `DeviceManager` channel `end` of the driver of device
/// `caller` until it closes. Each request runs as a task of its own, so
/// that one that waits does not hold up the others. `AddDevice` and
/// `RemoveDevice` are served; `PublishService` and `RequestResource`
/// fail with `NOT_SUPPORTED`.
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
                R::RequestResource(_, responder) => responder.reply(s, Err(Status::NotSupported)),
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
