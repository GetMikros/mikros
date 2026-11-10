//! What the requests of `Accounts` do: the checks each one makes, and
//! the changes it makes to the users.

use crate::checks::Checks;
use crate::lock::Lock;
use crate::store::Store;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::session::UserKind;
use mikros_rt::sys;
use mikros_userdb::{Attempts, Kind, LAST_ID, Refusal, Users, hash, valid_name};

/// The permissions that `Authenticate` and changes to other users'
/// accounts need, and the action an administrator authorizes for them.
const AUTHENTICATE: &str = "accounts.authenticate";
const MANAGE: &str = "accounts.manage";
/// The app ID that may create the first administrator.
const SETUP: &str = "system.setup";
const SECOND: i64 = 1_000_000_000;

pub struct Service {
    pub store: Store,
    pub checks: Checks,
    /// `/Users`, where users' home folders are.
    pub homes: Option<NodeAsyncClient>,
    pub reactor: Reactor,
    attempts: RefCell<Attempts>,
    /// For each name with an attempt under way, the lock that makes its
    /// attempts one at a time.
    names: RefCell<BTreeMap<String, Rc<Lock>>>,
    /// Makes changes to the users one at a time.
    changes: Lock,
}

/// A password, whose bytes are overwritten once it is dropped.
pub struct Secret(pub String);

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zeros are valid UTF-8.
        for b in unsafe { self.0.as_bytes_mut() } {
            // SAFETY: `b` is a valid, aligned byte of the string.
            unsafe { core::ptr::write_volatile(b, 0) };
        }
    }
}

impl Secret {
    fn bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Service {
    pub fn new(
        store: Store,
        checks: Checks,
        homes: Option<NodeAsyncClient>,
        reactor: Reactor,
    ) -> Service {
        Service {
            store,
            checks,
            homes,
            reactor,
            attempts: RefCell::new(Attempts::new()),
            names: RefCell::new(BTreeMap::new()),
            changes: Lock::new(),
        }
    }

    pub async fn authenticate(
        &self,
        client: &ClientIdentity,
        name: &str,
        password: Secret,
    ) -> Result<u32, Status> {
        if !self.checks.holds(client, AUTHENTICATE).await {
            return Err(Status::AccessDenied);
        }
        self.attempt(name, &password)
            .await
            .ok_or(Status::AccessDenied)
    }

    /// Checks `password` for the user `name`: the user's ID when it is
    /// that user's password. Attempts for one name are made one at a
    /// time, each after the delay that the name's failures in a row
    /// bring. An unknown name costs a hash too.
    async fn attempt(&self, name: &str, password: &Secret) -> Option<u32> {
        let lock = self
            .names
            .borrow_mut()
            .entry(String::from(name))
            .or_insert_with(|| Rc::new(Lock::new()))
            .clone();
        let held = lock.lock().await;
        let delay = self.attempts.borrow().delay(name);
        if delay > 0 {
            let _ = self
                .reactor
                .sleep_until(now() + delay as i64 * SECOND)
                .await;
        }
        let user = self
            .store
            .users()
            .by_name(name)
            .map(|r| (r.id, r.hash.clone()));
        let matched = match &user {
            Some((_, phc)) => hash::verify(password.bytes(), phc),
            None => {
                let salt = salt().unwrap_or_default();
                let _ = hash::hash(password.bytes(), &salt);
                false
            }
        };
        match matched {
            true => self.attempts.borrow_mut().succeeded(name),
            false => self.attempts.borrow_mut().failed(name),
        }
        drop(held);
        if !lock.busy() {
            self.names.borrow_mut().remove(name);
        }
        user.filter(|_| matched).map(|(id, _)| id)
    }

    /// Whether `client` holds `accounts.manage` and an administrator
    /// authorized it, asked for with `reason`.
    async fn may_manage(&self, client: &ClientIdentity, reason: String) -> bool {
        self.checks.holds(client, MANAGE).await
            && self.checks.authorized(client, MANAGE, reason).await
    }

    pub async fn create_user(
        &self,
        client: &ClientIdentity,
        name: &str,
        full_name: &str,
        kind: UserKind,
        password: Secret,
    ) -> Result<u32, Status> {
        let kind = kind_of(kind)?;
        if !valid_name(name) || full_name.is_empty() || password.0.is_empty() {
            return Err(Status::InvalidArgs);
        }
        let first =
            |users: &Users| users.is_empty() && client.app_id == SETUP && kind == Kind::Admin;
        let setup = first(&self.store.users());
        if !setup
            && !self
                .may_manage(client, format!("Create the user {name}"))
                .await
        {
            return Err(Status::AccessDenied);
        }
        let _changing = self.changes.lock().await;
        if setup && !first(&self.store.users()) {
            return Err(Status::AccessDenied);
        }
        if self.store.users().by_name(name).is_some() || self.home_exists(name).await? {
            return Err(Status::AlreadyExists);
        }
        if self.store.users().next_id() > LAST_ID {
            return Err(Status::NoResources);
        }
        // The home folder is a subvolume of its own, which only a file
        // system with subvolumes makes.
        Err(Status::NotSupported)
    }

    /// Whether `/Users/<name>` exists.
    async fn home_exists(&self, name: &str) -> Result<bool, Status> {
        let homes = self.homes.as_ref().ok_or(Status::NotSupported)?;
        let (_, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        let opened = homes
            .open(name.into(), OpenFlags::NO_FOLLOW, 0, Rights::READ, server)
            .await;
        match opened {
            Ok(Ok(_)) => Ok(true),
            Ok(Err(Error::NotFound)) => Ok(false),
            _ => Err(Status::Io),
        }
    }

    pub async fn delete_user(
        &self,
        client: &ClientIdentity,
        user: u32,
        delete_home: bool,
    ) -> Result<(), Status> {
        if !self
            .may_manage(client, format!("Delete the user {user}"))
            .await
        {
            return Err(Status::AccessDenied);
        }
        let _changing = self.changes.lock().await;
        let mut users = self.store.users().clone();
        users.delete(user).map_err(status)?;
        if delete_home {
            // The home folder is a subvolume, which only a file system
            // with subvolumes deletes.
            return Err(Status::NotSupported);
        }
        self.save(users).await
    }

    pub async fn set_password(
        &self,
        client: &ClientIdentity,
        user: u32,
        old_password: Secret,
        new_password: Secret,
    ) -> Result<(), Status> {
        if new_password.0.is_empty() {
            return Err(Status::InvalidArgs);
        }
        if client.user == user {
            let name = self.store.users().get(user).map(|r| r.name.clone());
            let name = name.ok_or(Status::NotFound)?;
            if self.attempt(&name, &old_password).await != Some(user) {
                return Err(Status::AccessDenied);
            }
        } else {
            let reason = format!("Change the password of the user {user}");
            if !self.may_manage(client, reason).await {
                return Err(Status::AccessDenied);
            }
        }
        let phc = hash::hash(new_password.bytes(), &salt()?).ok_or(Status::NoMemory)?;
        let _changing = self.changes.lock().await;
        let mut users = self.store.users().clone();
        users.set_hash(user, phc).map_err(status)?;
        self.save(users).await
    }

    pub async fn set_kind(
        &self,
        client: &ClientIdentity,
        user: u32,
        kind: UserKind,
    ) -> Result<(), Status> {
        let kind = kind_of(kind)?;
        let reason = format!("Change the kind of the user {user}");
        if !self.may_manage(client, reason).await {
            return Err(Status::AccessDenied);
        }
        let _changing = self.changes.lock().await;
        let mut users = self.store.users().clone();
        users.set_kind(user, kind).map_err(status)?;
        self.save(users).await
    }

    pub async fn set_full_name(
        &self,
        client: &ClientIdentity,
        user: u32,
        full_name: &str,
    ) -> Result<(), Status> {
        if full_name.is_empty() {
            return Err(Status::InvalidArgs);
        }
        let reason = format!("Change the full name of the user {user}");
        if client.user != user && !self.may_manage(client, reason).await {
            return Err(Status::AccessDenied);
        }
        let _changing = self.changes.lock().await;
        let mut users = self.store.users().clone();
        users.set_full_name(user, full_name).map_err(status)?;
        self.save(users).await
    }

    async fn save(&self, users: Users) -> Result<(), Status> {
        self.store
            .save(users, &self.reactor)
            .await
            .map_err(|_| Status::Io)
    }
}

/// The kind that `kind` names.
fn kind_of(kind: UserKind) -> Result<Kind, Status> {
    match kind {
        UserKind::Standard => Ok(Kind::Standard),
        UserKind::Admin => Ok(Kind::Admin),
        UserKind::Unknown(_) => Err(Status::InvalidArgs),
    }
}

/// The status of a refused change.
fn status(refusal: Refusal) -> Status {
    match refusal {
        Refusal::Invalid => Status::InvalidArgs,
        Refusal::Exists => Status::AlreadyExists,
        Refusal::NotFound => Status::NotFound,
        Refusal::LastAdmin => Status::BadState,
        Refusal::Exhausted => Status::NoResources,
    }
}

/// A random salt, unless the kernel has no random bytes to give.
fn salt() -> Result<[u8; hash::SALT_LEN], Status> {
    let mut salt = [0; hash::SALT_LEN];
    // SAFETY: the buffer is valid for its length.
    let status = unsafe { sys::system_get_random(salt.as_mut_ptr(), salt.len() as u64) };
    match status == mikros_rt::OK {
        true => Ok(salt),
        false => Err(Status::Internal),
    }
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
