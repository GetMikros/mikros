//! As the test disk driver: whether the device manager's
//! `mikros.driver.Removal` serves it as it should on the test machine,
//! whose one removable disk is the USB scratch disk.

use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{Removal, RemovalAsyncClient};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision, Grant};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const BROKER: &str = "mikros.permissions.Broker";
const REMOVAL: &str = "mikros.driver.Removal";
/// The permission that `Removal` needs, which the driver grants itself
/// for its own user and app ID through the broker.
const VOLUMES: &str = "files.volumes";

/// Grants this driver `files.volumes` with the broker, then checks
/// that `Removal` lists at most one removable disk, the USB scratch disk
/// with the serial number `mikros`, which may not have been added yet,
/// and fails to eject `device`, the driver's own partition, with
/// `NOT_FOUND`.
pub async fn works(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    user: u32,
    app_id: &str,
    device: u64,
) -> bool {
    let Ok((client, server)) = endpoints::<Broker>() else {
        return false;
    };
    let connected = directory
        .connect(BROKER.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return false;
    }
    let broker = BrokerAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
    let grant = Grant {
        user,
        app_id: app_id.into(),
        permission: VOLUMES.into(),
        decision: Decision::Allowed,
    };
    if !matches!(broker.set_grant(grant).await, Ok(Ok(_))) {
        return false;
    }
    let Ok((client, server)) = endpoints::<Removal>() else {
        return false;
    };
    let connected = directory
        .connect(REMOVAL.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return false;
    }
    let removal = RemovalAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
    let listed = match removal.list().await {
        Ok(listed) => listed.disks,
        Err(_) => return false,
    };
    let usb_only = listed.len() <= 1 && listed.iter().all(|d| d.serial == "mikros");
    let refused = matches!(removal.eject(device).await, Ok(Err(Status::NotFound)));
    if !(usb_only && refused) {
        mikros_rt::log!(
            Error,
            "test-disk: Removal listed {} disks; ejecting device {} refused: {}",
            listed.len(),
            device,
            refused
        );
    }
    usb_only && refused
}
