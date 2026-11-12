//! The drivers on the root file system: `mikros.driver.RootDrivers`,
//! which the service manager calls once with the root's `/usr`.

use crate::drivers;
use crate::supervise::{self, Manager};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::DriverKind;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    RootDrivers, RootDriversAsyncServer, RootDriversRequest, RootDriversRootAvailableResponse,
};
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;

pub const PROTOCOL: &str = "mikros.driver.RootDrivers";
/// The only caller that may connect.
pub const CALLER: &str = "system.servicemgr";
/// The drivers' directory below `/usr`.
const DRIVERS: &str = "lib/drivers";

/// Serves `RootDrivers` on `end` until it closes. After the first
/// `RootAvailable`, on any connection, others fail with `BAD_STATE`.
pub async fn serve(m: Rc<Manager>, end: ServerEnd<RootDrivers>) {
    let server = RootDriversAsyncServer::new(end, m.reactor.clone());
    while let Ok(RootDriversRequest::RootAvailable(request, responder)) = server.next().await {
        let reply = match m.root_drivers.replace(true) {
            true => Err(Status::BadState),
            false => {
                let usr = NodeAsyncClient::new(request.usr, m.reactor.clone());
                add_drivers(&m, &usr).await;
                Ok(RootDriversRootAvailableResponse {})
            }
        };
        let _ = responder.reply(server.server(), reply);
    }
}

/// Reads the driver manifests of the root's `usr`, keeps those whose
/// names are not taken, binds them to the devices without a driver, and
/// has them take over the devices that a fallback driver runs.
async fn add_drivers(m: &Rc<Manager>, usr: &NodeAsyncClient) {
    let read = drivers::read_all(usr, DRIVERS, &m.reactor).await;
    let added = {
        let mut known = m.drivers.borrow_mut();
        let mut added = 0;
        for driver in read {
            if known.iter().any(|k| k.name == driver.name) {
                mikros_rt::log!(
                    Info,
                    "device-manager: the root's driver {} is not used, as the boot image has one of that name",
                    driver.name
                );
                continue;
            }
            known.push(driver);
            added += 1;
        }
        known.sort_by(|a, b| a.name.cmp(&b.name));
        added
    };
    mikros_rt::log!(
        Info,
        "device-manager: read {added} more driver manifests from the root file system"
    );
    if added == 0 {
        return;
    }
    let bound = {
        let drivers = m.drivers.borrow();
        let mut devices = m.devices.borrow_mut();
        let unbound = devices.unbound();
        unbound
            .into_iter()
            .filter(|&id| devices.bind_device(id, &drivers))
            .collect::<Vec<u64>>()
    };
    for id in bound {
        m.spawner.spawn(supervise::supervise(m.clone(), id));
    }
    // In tasks of their own, so that the reply does not wait for the
    // fallback drivers to stop.
    for (id, normal) in takeovers(m) {
        m.spawner.spawn(take_over(m.clone(), id, normal));
    }
}

/// The devices whose running driver is a fallback driver while a normal
/// driver matches them now, with that normal driver's name.
fn takeovers(m: &Manager) -> Vec<(u64, String)> {
    let drivers = m.drivers.borrow();
    let kind = |name: &str| drivers.iter().find(|d| d.name == name).map(|d| d.kind);
    m.devices
        .borrow()
        .running()
        .into_iter()
        .filter(|(_, running, _)| kind(running) == Some(DriverKind::Fallback))
        .filter_map(|(id, _, hardware_ids)| {
            let choice = mikros_manifest::choose(&drivers, &hardware_ids)?;
            (choice.driver.kind == DriverKind::Normal).then(|| (id, choice.driver.name.clone()))
        })
        .collect()
}

/// Binds the normal driver `normal` to device `id` in place of its
/// fallback driver, which is stopped as a removed device's driver is,
/// after the devices below it are removed. The device's supervisor then
/// starts `normal`.
async fn take_over(m: Rc<Manager>, id: u64, normal: String) {
    let children = m.devices.borrow().children(id);
    for child in children {
        supervise::remove(m.clone(), child).await;
    }
    let (client, pid) = match m.devices.borrow_mut().get_mut(id) {
        Some(device) => {
            mikros_rt::log!(
                Info,
                "device-manager: driver {normal} takes over device {id}, {}, from driver {}",
                device.name,
                device.driver.as_deref().unwrap_or("")
            );
            device.driver = Some(normal);
            device.taken_over = true;
            (device.client.clone(), device.pid)
        }
        None => return,
    };
    supervise::stop(&m, client, pid).await;
}
