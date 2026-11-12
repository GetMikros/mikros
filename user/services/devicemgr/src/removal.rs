//! `mikros.driver.Removal`, which the service directory forwards
//! through the device manager's provider to callers with the permission
//! its manifest names: listing the removable disks, and ejecting one by
//! removing every device below it.

use crate::root;
use crate::shutdown;
use crate::supervise::{self, EJECT_STOP_LIMIT, Manager};
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    RemovableDisk, Removal, RemovalAsyncServer, RemovalEjectResponse, RemovalListResponse,
    RemovalRequest,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderAsyncServer, ProviderRequest};

/// The protocol, the hardware ID of disks, and the properties that say
/// whether a disk is removable and what its serial number is.
const PROTOCOL: &str = "mikros.driver.Removal";
const DISK: &str = "storage:disk";
const REMOVABLE: &str = "removable";
const SERIAL: &str = "serial";

/// Serves the provider `end` until it closes: each connection without an
/// instance name gets `Removal` for this protocol, and
/// `mikros.driver.Shutdown` for that one, and the service manager's
/// alone `mikros.driver.RootDrivers`. Others are dropped, which closes
/// them.
pub async fn serve(m: Rc<Manager>, end: ServerEnd<Provider>) {
    let provider = ProviderAsyncServer::new(end, m.reactor.clone());
    while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
        if c.instance.is_some() {
            continue;
        }
        if c.protocol == PROTOCOL {
            let connection = serve_removal(m.clone(), ServerEnd::new(c.server));
            m.spawner.spawn(connection);
        } else if c.protocol == shutdown::PROTOCOL {
            let connection = shutdown::serve_kind(m.clone(), ServerEnd::new(c.server));
            m.spawner.spawn(connection);
        } else if c.protocol == root::PROTOCOL && c.client.app_id == root::CALLER {
            let connection = root::serve(m.clone(), ServerEnd::new(c.server));
            m.spawner.spawn(connection);
        }
    }
}

/// Serves `Removal` on `end` until it closes. Each `Eject` runs as a
/// task of its own, so that `List` is answered while it waits.
async fn serve_removal(m: Rc<Manager>, end: ServerEnd<Removal>) {
    let server = Rc::new(RemovalAsyncServer::new(end, m.reactor.clone()));
    while let Ok(request) = server.next().await {
        match request {
            RemovalRequest::List(_, responder) => {
                let disks = removable_disks(&m);
                let _ = responder.reply(server.server(), RemovalListResponse { disks });
            }
            RemovalRequest::Eject(request, responder) => {
                let (m, server) = (m.clone(), server.clone());
                m.spawner.clone().spawn(async move {
                    let ejected = eject(&m, request.id).await;
                    let reply = ejected.map(|()| RemovalEjectResponse {});
                    let _ = responder.reply(server.server(), reply);
                });
            }
        }
    }
}

/// Whether the device has the hardware ID `storage:disk` and the
/// property `removable` with the value `true`.
fn is_removable_disk(device: &crate::devices::Device) -> bool {
    device.hardware_ids.iter().any(|id| id == DISK) && property(device, REMOVABLE) == Some("true")
}

fn property<'a>(device: &'a crate::devices::Device, key: &str) -> Option<&'a str> {
    let (_, value) = device.properties.iter().find(|(k, _)| k == key)?;
    Some(value)
}

/// The removable disks, at most 32, with their IDs, names and serial
/// numbers, an empty one when a disk has no `serial` property.
fn removable_disks(m: &Manager) -> Vec<RemovableDisk> {
    let devices = m.devices.borrow();
    devices
        .iter()
        .filter(|d| !d.removing && is_removable_disk(d))
        .take(32)
        .map(|d| RemovableDisk {
            id: d.id,
            name: d.name.clone(),
            serial: property(d, SERIAL).unwrap_or_default().into(),
        })
        .collect()
}

/// Removes every device below the removable disk `id`, children before
/// their parents, waiting up to `EJECT_STOP_LIMIT` for each `Stop`, and
/// keeps the disk itself. Fails with `NOT_FOUND` when `id` is not a
/// removable disk.
async fn eject(m: &Rc<Manager>, id: u64) -> Result<(), Status> {
    let children = {
        let devices = m.devices.borrow();
        let disk = devices.get(id).ok_or(Status::NotFound)?;
        if disk.removing || !is_removable_disk(disk) {
            return Err(Status::NotFound);
        }
        devices.children(id)
    };
    for child in children {
        supervise::remove_within(m.clone(), child, EJECT_STOP_LIMIT).await;
    }
    Ok(())
}
