//! What a user's namespace needs from other services: the user's name,
//! from the accounts service, views that merge directories, from the
//! union service, and new subvolumes, from the root file system's
//! subvolume management. All are reached through the process manager's
//! own directory.

use crate::processes::Shared;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::btrfs::{Subvolumes, SubvolumesAsyncClient};
use mikros_rt::interfaces::mikros::fs::{Error, Layer, Node, Rights, Union, UnionAsyncClient};
use mikros_rt::interfaces::mikros::session::{Accounts, AccountsAsyncClient};

const ACCOUNTS: &str = "mikros.session.Accounts";
const UNION: &str = "mikros.fs.Union";
/// The root file system's subvolume management, and its instance.
const SUBVOLUMES: &str = "device.subvolumes";
const ROOT: &str = "root";

/// A connection to `protocol` through the process manager's directory.
async fn connect<P>(shared: &Shared, protocol: &str) -> Option<ClientEnd<P>> {
    connect_to(shared, protocol, None).await
}

/// A connection to `instance`, or any instance, of `protocol` through
/// the process manager's directory.
async fn connect_to<P>(
    shared: &Shared,
    protocol: &str,
    instance: Option<&str>,
) -> Option<ClientEnd<P>> {
    let directory = shared.permissions.directory()?;
    let (client, server) = endpoints::<P>().ok()?;
    let connected = directory
        .connect(
            protocol.into(),
            instance.map(String::from),
            server.into_handle(),
        )
        .await;
    matches!(connected, Ok(Ok(_))).then_some(client)
}

/// Makes `path`, from the root of the root file system, a new subvolume.
/// Fails with `NOT_SUPPORTED` when the root file system has no subvolume
/// management to reach, as tmpfs has not.
pub async fn subvolume(shared: &Shared, path: &str) -> Result<(), Status> {
    let client = connect_to::<Subvolumes>(shared, SUBVOLUMES, Some(ROOT))
        .await
        .ok_or(Status::NotSupported)?;
    let subvolumes = SubvolumesAsyncClient::new(client, shared.reactor.clone());
    match subvolumes.create(path.into()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(Error::NotSupported)) => Err(Status::NotSupported),
        Ok(Err(e)) => Err(crate::namespace::status(e)),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// The name of user `user`, from the record that `ListUsers` of the
/// accounts service returns for it. Fails with `NOT_FOUND` when there is
/// none, or the accounts service cannot be reached.
pub async fn name(shared: &Shared, user: u32) -> Result<String, Status> {
    let client = connect::<Accounts>(shared, ACCOUNTS)
        .await
        .ok_or(Status::NotFound)?;
    let accounts = AccountsAsyncClient::new(client, shared.reactor.clone());
    let mut after = 0;
    loop {
        let users = accounts
            .list_users(after)
            .await
            .map_err(|_| Status::NotFound)?
            .users;
        if let Some(found) = users.iter().find(|u| u.id == user) {
            return Ok(found.name.clone());
        }
        match users.last() {
            Some(last) if last.id > after => after = last.id,
            _ => return Err(Status::NotFound),
        }
    }
}

/// A read-only view with `rights` that merges `layers`, the first of
/// which a name resolves to first.
pub async fn merged(
    shared: &Shared,
    layers: Vec<ClientEnd<Node>>,
    rights: Rights,
) -> Result<ClientEnd<Node>, Status> {
    let layers = layers
        .into_iter()
        .map(|directory| Layer {
            directory,
            writable: false,
        })
        .collect();
    view(shared, layers, rights).await
}

/// A view with `rights` of `layers`, the first of which a name resolves
/// to first, and which alone may be writable.
pub async fn view(
    shared: &Shared,
    layers: Vec<Layer>,
    rights: Rights,
) -> Result<ClientEnd<Node>, Status> {
    let client = connect::<Union>(shared, UNION)
        .await
        .ok_or(Status::NotFound)?;
    let union = UnionAsyncClient::new(client, shared.reactor.clone());
    let (view, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    match union.create(layers, rights, server).await {
        Ok(Ok(_)) => Ok(view),
        Ok(Err(_)) => Err(Status::Io),
        Err(_) => Err(Status::PeerClosed),
    }
}
