//! Asking the permission broker whether a process holds a permission.
//!
//! The process manager connects to the broker through its own directory
//! once the service manager has bound it. Until the broker has answered
//! on that connection, only the service manager that init asked for
//! holds a permission, `system.identity.set`, so that it can start the
//! broker. From then on the broker decides for every process, the
//! service manager included. When the connection fails, the broker is
//! gone and that service manager holds `system.identity.set` again. The broker's
//! `AppNumbers` gives the numbers of app IDs, which the process manager
//! keeps once given, as they never change.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::permissions::{
    AppNumbers, AppNumbersAsyncClient, Broker, BrokerAsyncClient, Decision,
};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, Directory, DirectoryAsyncClient};

/// The broker's protocols.
const PROTOCOL: &str = "mikros.permissions.Broker";
const APP_NUMBERS: &str = "mikros.permissions.AppNumbers";
pub const SET_IDENTITY: &str = "system.identity.set";
pub const FULL_DISK: &str = "user.full_disk.access";
pub const SIGNAL_ANY: &str = "system.processes.control";
pub const LIST_ALL: &str = "system.processes.read";
pub const SET_USER: &str = "system.user_id.set";
/// The permission asked about, for the process manager itself, to learn
/// that the broker serves requests. Any answer will do.
const PROBE: &str = "system.permissions.read";

/// The process manager's directory and connection to the broker, once
/// it made them, and whether the broker has answered on it.
#[derive(Default)]
pub struct Permissions {
    directory: RefCell<Option<Rc<DirectoryAsyncClient>>>,
    broker: RefCell<Option<Rc<BrokerAsyncClient>>>,
    serving: Cell<bool>,
    numbers: RefCell<Option<Rc<AppNumbersAsyncClient>>>,
    /// The numbers of app IDs that the broker gave.
    known: RefCell<BTreeMap<String, u32>>,
}

impl Permissions {
    /// Connects to the broker through `directory`, the process manager's
    /// own, and asks it about a permission of the process manager `me`,
    /// so that it is known to serve once it answers. Without a broker in
    /// the directory, no connection is made.
    pub async fn connect(
        &self,
        directory: ClientEnd<Directory>,
        reactor: &Reactor,
        me: ClientIdentity,
    ) {
        let directory = Rc::new(DirectoryAsyncClient::new(directory, reactor.clone()));
        *self.directory.borrow_mut() = Some(directory.clone());
        let Ok((client, server)) = endpoints::<Broker>() else {
            return;
        };
        let connected = directory
            .connect(PROTOCOL.into(), None, server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            return;
        }
        let broker = Rc::new(BrokerAsyncClient::new(client, reactor.clone()));
        *self.broker.borrow_mut() = Some(broker.clone());
        if let Ok((client, server)) = endpoints::<AppNumbers>() {
            let connected = directory
                .connect(APP_NUMBERS.into(), None, server.into_handle())
                .await;
            if matches!(connected, Ok(Ok(_))) {
                let numbers = AppNumbersAsyncClient::new(client, reactor.clone());
                *self.numbers.borrow_mut() = Some(Rc::new(numbers));
            }
        }
        let answered = broker.check(me, PROBE.into()).await;
        self.answered(answered.is_ok());
    }

    /// The number of `app_id` (MIKSPEC-0037.R31), 0 when the broker gives
    /// none.
    pub async fn app_number(&self, app_id: &str) -> u32 {
        if let Some(&number) = self.known.borrow().get(app_id) {
            return number;
        }
        let Some(numbers) = self.numbers.borrow().clone() else {
            return 0;
        };
        match numbers.number(app_id.into()).await {
            Ok(Ok(r)) => {
                self.known.borrow_mut().insert(app_id.into(), r.number);
                r.number
            }
            _ => 0,
        }
    }

    /// The process manager's own directory, once it connected to it.
    pub fn directory(&self) -> Option<Rc<DirectoryAsyncClient>> {
        self.directory.borrow().clone()
    }

    /// Whether `client` holds `permission`, where `bootstrap` says that it
    /// is the service manager that init asked for.
    pub async fn holds(&self, client: ClientIdentity, bootstrap: bool, permission: &str) -> bool {
        if bootstrap && !self.serving.get() {
            return permission == SET_IDENTITY;
        }
        let Some(broker) = self.broker.borrow().clone() else {
            return false;
        };
        let checked = broker.check(client, permission.into()).await;
        self.answered(checked.is_ok());
        matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
    }

    /// A connection of its own to the broker, for watching permissions,
    /// once the process manager has its directory.
    pub async fn watcher(&self, reactor: &Reactor) -> Option<BrokerAsyncClient> {
        let directory = self.directory.borrow().clone()?;
        let (client, server) = endpoints::<Broker>().ok()?;
        let connected = directory
            .connect(PROTOCOL.into(), None, server.into_handle())
            .await;
        matches!(connected, Ok(Ok(_))).then(|| BrokerAsyncClient::new(client, reactor.clone()))
    }

    /// Records whether the broker answered on its connection; one that
    /// failed is dropped.
    fn answered(&self, answered: bool) {
        self.serving.set(answered);
        if !answered {
            *self.broker.borrow_mut() = None;
        }
    }
}
