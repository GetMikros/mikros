//! Asking the permission broker whether a process holds a permission.
//!
//! The process manager connects to the broker through its own directory
//! once the service manager has bound it. Until the broker has answered
//! on that connection, only the service manager that init asked for
//! holds a permission, `process.set_identity`, so that it can start the
//! broker. From then on the broker decides for every process, the
//! service manager included. When the connection fails, the broker is
//! gone and that service manager holds `process.set_identity` again.

use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, Directory, DirectoryAsyncClient};

/// The broker's protocol.
const PROTOCOL: &str = "mikros.permissions.Broker";
pub const SET_IDENTITY: &str = "process.set_identity";
pub const FULL_DISK: &str = "files.full_disk";
pub const SIGNAL_ANY: &str = "process.signal_any";
/// The permission asked about, for the process manager itself, to learn
/// that the broker serves requests. Any answer will do.
const PROBE: &str = "permissions.check";

/// The process manager's directory and connection to the broker, once
/// it made them, and whether the broker has answered on it.
#[derive(Default)]
pub struct Permissions {
    directory: RefCell<Option<Rc<DirectoryAsyncClient>>>,
    broker: RefCell<Option<Rc<BrokerAsyncClient>>>,
    serving: Cell<bool>,
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
        let answered = broker.check(me, PROBE.into()).await;
        self.answered(answered.is_ok());
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

    /// Records whether the broker answered on its connection; one that
    /// failed is dropped.
    fn answered(&self, answered: bool) {
        self.serving.set(answered);
        if !answered {
            *self.broker.borrow_mut() = None;
        }
    }
}
