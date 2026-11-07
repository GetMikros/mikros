//! What the requests of `Accounts` do and the checks each one makes.

use crate::checks::Checks;
use crate::lock::Lock;
use crate::store::Store;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::sys;
use mikros_userdb::{Attempts, hash};

/// The permission that `Authenticate` needs.
const AUTHENTICATE: &str = "accounts.authenticate";
const SECOND: i64 = 1_000_000_000;

pub struct Service {
    pub store: Store,
    pub checks: Checks,
    pub reactor: Reactor,
    attempts: RefCell<Attempts>,
    /// For each name with an attempt under way, the lock that makes its
    /// attempts one at a time.
    names: RefCell<BTreeMap<String, Rc<Lock>>>,
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
    pub fn new(store: Store, checks: Checks, reactor: Reactor) -> Service {
        Service {
            store,
            checks,
            reactor,
            attempts: RefCell::new(Attempts::new()),
            names: RefCell::new(BTreeMap::new()),
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
