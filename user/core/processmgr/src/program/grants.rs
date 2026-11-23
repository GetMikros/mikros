//! Folder grants (MIKSPEC-0050.R38): an app that holds a folder's access
//! permission gets the folder at its real path through a revocable view
//! of the union server (MIKSPEC-0051.R9), whose key belongs to the app,
//! its user and the permission. The process manager watches those
//! permissions with confirmation, revokes the key on
//! `OnRevokedConfirmed`, so that every connection and shared mapping
//! made through it ends, and then confirms (MIKSPEC-0037.R34). A view
//! made under a key that was revoked meanwhile is dropped, so that no
//! namespace built across the revocation keeps it.

use crate::program::pending::Gate;
use crate::program::processes::Shared;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, Rights, Union, UnionAsyncClient};
use mikros_rt::interfaces::mikros::permissions::{BrokerAsyncClient, BrokerEvent};

const UNION: &str = "mikros.fs.Union";
/// The folders of a user's home that a permission grants, and the
/// permission.
pub const FOLDERS: [(&str, &str); 7] = [
    ("user.desktop.access", "desktop"),
    ("user.documents.access", "documents"),
    ("user.downloads.access", "downloads"),
    ("user.pictures.access", "pictures"),
    ("user.music.access", "music"),
    ("user.videos.access", "videos"),
    ("user.developer.access", "developer"),
];
pub const FULL_DISK: &str = "user.full_disk.access";
pub const VOLUMES: &str = "user.volumes.access";
/// The first key; keys are never used again once revoked.
const FIRST_KEY: u64 = 1 << 40;

/// A grant: the user, the app ID and the permission.
type GrantId = (u32, String, String);

/// The current revocation key of each grant, by user, app and
/// permission.
#[derive(Default)]
pub struct Grants {
    keys: RefCell<BTreeMap<(u32, String, String), u64>>,
    next: Cell<u64>,
    /// The gates of the pending entries of each grant, which a
    /// revocation closes (MIKSPEC-0094.R11).
    gates: RefCell<BTreeMap<GrantId, Vec<Weak<Gate>>>>,
}

impl Grants {
    /// The key of the grant of `permission` to `app_id` for `user`, made
    /// when it has none.
    fn key(&self, user: u32, app_id: &str, permission: &str) -> u64 {
        let id = (user, String::from(app_id), String::from(permission));
        if let Some(&key) = self.keys.borrow().get(&id) {
            return key;
        }
        let key = FIRST_KEY + self.next.get();
        self.next.set(self.next.get() + 1);
        self.keys.borrow_mut().insert(id, key);
        key
    }

    /// Adds `gate` as a gate of that grant.
    pub fn add_gate(&self, user: u32, app_id: &str, permission: &str, gate: Weak<Gate>) {
        let id = (user, String::from(app_id), String::from(permission));
        let mut gates = self.gates.borrow_mut();
        let list = gates.entry(id).or_default();
        list.retain(|g| g.strong_count() > 0);
        list.push(gate);
    }

    /// Revokes the gates of that grant, so that no call that is waiting
    /// or starts afterwards goes on, and none asks again.
    fn revoke_gates(&self, user: u32, app_id: &str, permission: &str) {
        let id = (user, String::from(app_id), String::from(permission));
        let list = self.gates.borrow_mut().remove(&id).unwrap_or_default();
        for gate in list.iter().filter_map(Weak::upgrade) {
            gate.revoke();
        }
    }

    /// Whether `key` is still the key of that grant.
    fn current(&self, user: u32, app_id: &str, permission: &str, key: u64) -> bool {
        let id = (user, String::from(app_id), String::from(permission));
        self.keys.borrow().get(&id) == Some(&key)
    }

    /// Forgets the key of that grant, and returns it.
    fn revoked(&self, user: u32, app_id: &str, permission: &str) -> Option<u64> {
        let id = (user, String::from(app_id), String::from(permission));
        self.keys.borrow_mut().remove(&id)
    }
}

async fn union(shared: &Shared) -> Option<UnionAsyncClient> {
    let client = crate::program::users::connect::<Union>(shared, UNION).await?;
    Some(UnionAsyncClient::new(client, shared.reactor.clone()))
}

/// A revocable view with `rights` of `directory` for the grant of
/// `permission` to `app_id` for `user`; none when the union server
/// cannot make it, so that the namespace goes without the folder, or
/// when the grant was revoked meanwhile.
pub async fn view(
    shared: &Shared,
    (user, app_id, permission): (u32, &str, &str),
    directory: ClientEnd<Node>,
    rights: Rights,
) -> Option<ClientEnd<Node>> {
    let grants = &shared.grants;
    let key = grants.key(user, app_id, permission);
    let made = async {
        let union = union(shared).await.ok_or(Status::NotFound)?;
        let (view, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        match union.create_revocable(directory, rights, key, server).await {
            Ok(Ok(_)) => Ok(view),
            Ok(Err(_)) => Err(Status::Io),
            Err(_) => Err(Status::PeerClosed),
        }
    };
    match made.await {
        Ok(view) => grants
            .current(user, app_id, permission, key)
            .then_some(view),
        Err(e) => {
            mikros_rt::log!(
                Warning,
                "process-manager: no view for {permission} of {app_id} for user {user}: {e:?}"
            );
            None
        }
    }
}

/// Watches every permission of a folder grant with confirmation through
/// `broker`, a connection of its own, and on each revocation revokes the
/// grant's key and confirms, until the connection closes.
pub async fn watch(shared: Rc<Shared>, broker: BrokerAsyncClient) {
    let permissions: Vec<&str> = FOLDERS
        .iter()
        .map(|(p, _)| *p)
        .chain([FULL_DISK, VOLUMES])
        .collect();
    for permission in permissions {
        if !matches!(broker.watch_confirmed(permission.into()).await, Ok(Ok(_))) {
            mikros_rt::log!(Error, "process-manager: watching {permission} failed");
        }
    }
    while let Ok(event) = broker.next_event().await {
        let BrokerEvent::OnRevokedConfirmed(e) = event else {
            continue;
        };
        // Before anything waits for the union server, so that no call
        // that arrives meanwhile asks again.
        shared.grants.revoke_gates(e.user, &e.app_id, &e.permission);
        if let Some(key) = shared.grants.revoked(e.user, &e.app_id, &e.permission) {
            let revoked = match union(&shared).await {
                Some(union) => matches!(union.revoke(key).await, Ok(Ok(_))),
                None => false,
            };
            if !revoked {
                mikros_rt::log!(
                    Error,
                    "process-manager: revoking {} of {} for user {} failed",
                    e.permission,
                    e.app_id,
                    e.user
                );
            }
        }
        let _ = broker.revocation_done(e.sequence).await;
    }
}

/// The folders that each permission grants to a user named `name`: the
/// path, the rights, and the permission.
pub fn folders(name: &str) -> Vec<(String, Rights, &'static str)> {
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let mut all: Vec<(String, Rights, &'static str)> = FOLDERS
        .iter()
        .map(|(permission, folder)| (format!("/users/{name}/{folder}"), rw, *permission))
        .collect();
    all.push((format!("/users/{name}"), rw, FULL_DISK));
    all.push((String::from("/apps"), Rights::READ, FULL_DISK));
    all
}
