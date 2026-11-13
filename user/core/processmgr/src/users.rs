//! What a user's namespace needs from other services: the user's name,
//! from the accounts service, and views that merge directories, from
//! the union service. Both are reached through the process manager's
//! own directory.

use crate::processes::Shared;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Layer, Node, Rights, Union, UnionAsyncClient};
use mikros_rt::interfaces::mikros::session::{Accounts, AccountsAsyncClient};

const ACCOUNTS: &str = "mikros.session.Accounts";
const UNION: &str = "mikros.fs.Union";

/// A connection to `protocol` through the process manager's directory.
async fn connect<P>(shared: &Shared, protocol: &str) -> Option<ClientEnd<P>> {
    let directory = shared.permissions.directory()?;
    let (client, server) = endpoints::<P>().ok()?;
    let connected = directory
        .connect(protocol.into(), None, server.into_handle())
        .await;
    matches!(connected, Ok(Ok(_))).then_some(client)
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
    let client = connect::<Union>(shared, UNION)
        .await
        .ok_or(Status::NotFound)?;
    let union = UnionAsyncClient::new(client, shared.reactor.clone());
    let layers = layers
        .into_iter()
        .map(|directory| Layer {
            directory,
            writable: false,
        })
        .collect();
    let (view, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    match union.create(layers, rights, server).await {
        Ok(Ok(_)) => Ok(view),
        Ok(Err(_)) => Err(Status::Io),
        Err(_) => Err(Status::PeerClosed),
    }
}
