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
use mikros_rt::interfaces::mikros::session::{LoginMode, UserKind};
use mikros_rt::objects;
use mikros_userdb::{Attempts, Kind, LAST_ID, Refusal, Users, hash, valid_name};

/// The permissions that `Authenticate` and changes to other users'
/// accounts need; an administrator may allow the second.
const AUTHENTICATE: &str = "system.users.authenticate";
const MANAGE: &str = "admin.accounts.manage";
/// The app ID that may create the first administrator.
const SETUP: &str = "system.setup";
const SECOND: i64 = 1_000_000_000;

pub struct Service {
    pub store: Store,
    pub checks: Checks,
    /// `/users`, where users' home folders are.
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
        let mut bytes = core::mem::take(&mut self.0).into_bytes();
        objects::wipe(&mut bytes);
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

    /// Whether the broker allows `client` `admin.accounts.manage` when
    /// asked for it with `reason`.
    async fn may_manage(&self, client: &ClientIdentity, reason: String) -> bool {
        self.checks.requested(client, MANAGE, reason).await
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
        // system with subvolumes makes. The record is added last, so that
        // a failure before it leaves no user.
        let homes = self.homes.as_ref().ok_or(Status::NotSupported)?;
        let subvolumes = crate::homes::subvolumes(self.checks.directory(), &self.reactor).await?;
        let phc = hash::hash(password.0.as_bytes(), &salt()?).ok_or(Status::NoMemory)?;
        // The user gets the next ID, which owns the home folder.
        let owner = self.store.users().next_id();
        crate::homes::create(&subvolumes, homes, name, owner, &self.reactor).await?;
        let mut users = self.store.users().clone();
        let id = users.create(name, full_name, kind, phc).map_err(status)?;
        self.save(users).await?;
        Ok(id)
    }

    /// Makes the home folders of the existing users, and the folders in
    /// them, the users' own where they have another owner, and, once for
    /// each user, everything in them that user 0 owns. A failure for one
    /// user is logged, and the others are still done.
    pub async fn own_homes(&self) {
        let Some(homes) = self.homes.as_ref() else {
            return;
        };
        let users: alloc::vec::Vec<(u32, alloc::string::String)> = {
            let all = self.store.users();
            let mut found = alloc::vec::Vec::new();
            let mut after = 0;
            loop {
                let page: alloc::vec::Vec<_> =
                    all.list(after).map(|r| (r.id, r.name.clone())).collect();
                let Some(&(last, _)) = page.last() else {
                    break;
                };
                found.extend(page);
                after = last;
            }
            found
        };
        for (id, name) in users {
            if let Err(e) = crate::homes::own(homes, &name, id, &self.reactor).await {
                mikros_rt::log!(
                    Error,
                    "accounts: making /users/{name} the user's own failed: {e:?}"
                );
                continue;
            }
            // Once for each user, everything in it that user 0 owns.
            if self.store.owned(id) {
                continue;
            }
            let walked = crate::homes::own_all(homes, &name, id, &self.reactor).await;
            let recorded = match walked {
                Ok(()) => self
                    .store
                    .mark_owned(id, &self.reactor)
                    .await
                    .map_err(|_| Status::Io),
                Err(e) => Err(e),
            };
            if let Err(e) = recorded {
                mikros_rt::log!(
                    Error,
                    "accounts: making the files in /users/{name} the user's own failed: {e:?}"
                );
            }
        }
    }

    /// Whether `/users/<name>` exists.
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
        let deleted = users.delete(user).map_err(status)?;
        if !delete_home {
            return self.save(users).await;
        }
        // The home folder is a subvolume, which only a file system with
        // subvolumes deletes; without one, nothing is deleted.
        let subvolumes = crate::homes::subvolumes(self.checks.directory(), &self.reactor).await?;
        self.save(users).await?;
        crate::homes::delete(&subvolumes, &deleted.name).await
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

    /// The login mode of `user`.
    pub fn login_mode(&self, user: u32) -> Result<LoginMode, Status> {
        let users = self.store.users();
        let record = users.get(user).ok_or(Status::NotFound)?;
        Ok(match record.login {
            mikros_userdb::LoginMode::Console => LoginMode::Console,
            mikros_userdb::LoginMode::Desktop => LoginMode::Desktop,
        })
    }

    /// Sets the login mode of `user`, which needs `admin.accounts.manage`
    /// unless `client` runs as that user.
    pub async fn set_login_mode(
        &self,
        client: &ClientIdentity,
        user: u32,
        mode: LoginMode,
    ) -> Result<(), Status> {
        let mode = match mode {
            LoginMode::Console => mikros_userdb::LoginMode::Console,
            LoginMode::Desktop => mikros_userdb::LoginMode::Desktop,
            LoginMode::Unknown(_) => return Err(Status::InvalidArgs),
        };
        if self.store.users().get(user).is_none() {
            return Err(Status::NotFound);
        }
        let reason = format!("Change the login mode of the user {user}");
        if client.user != user && !self.may_manage(client, reason).await {
            return Err(Status::AccessDenied);
        }
        let _changing = self.changes.lock().await;
        let mut users = self.store.users().clone();
        users.set_login_mode(user, mode).map_err(status)?;
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
    objects::random(&mut salt).map_err(|_| Status::Internal)?;
    Ok(salt)
}

fn now() -> i64 {
    objects::now()
}
