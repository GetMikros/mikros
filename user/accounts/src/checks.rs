//! Whether a client holds a permission, which the permission broker
//! decides, and whether an administrator authorized an action of a
//! client, through the session manager's `Authorization`.

use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, DirectoryAsyncClient};
use mikros_rt::interfaces::mikros::session::{Authorization, AuthorizationAsyncClient};

const BROKER: &str = "mikros.permissions.Broker";
const AUTHORIZATION: &str = "mikros.session.Authorization";

pub struct Checks {
    directory: DirectoryAsyncClient,
    /// The connection to the broker, once made, until it fails.
    broker: RefCell<Option<Rc<BrokerAsyncClient>>>,
    reactor: Reactor,
}

impl Checks {
    /// Checks through the service's `directory`.
    pub fn new(directory: DirectoryAsyncClient, reactor: Reactor) -> Checks {
        Checks {
            directory,
            broker: RefCell::new(None),
            reactor,
        }
    }

    /// Whether the broker allows `client` `permission`. Without an
    /// answer, it does not.
    pub async fn holds(&self, client: &ClientIdentity, permission: &str) -> bool {
        let Some(broker) = self.broker().await else {
            return false;
        };
        match broker.check(copy(client), permission.into()).await {
            Ok(Ok(checked)) => checked.decision == Decision::Allowed,
            Ok(Err(_)) => false,
            Err(_) => {
                *self.broker.borrow_mut() = None;
                false
            }
        }
    }

    async fn broker(&self) -> Option<Rc<BrokerAsyncClient>> {
        if let Some(broker) = self.broker.borrow().clone() {
            return Some(broker);
        }
        let (client, server) = endpoints::<Broker>().ok()?;
        let connected = self
            .directory
            .connect(BROKER.into(), None, server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            return None;
        }
        let broker = Rc::new(BrokerAsyncClient::new(client, self.reactor.clone()));
        *self.broker.borrow_mut() = Some(broker.clone());
        Some(broker)
    }

    /// Whether an administrator authorized `action` of `client`, asked
    /// for with `reason`. Each request makes a connection of its own, so
    /// that a restarted session manager answers the next one. Without an
    /// answer, nothing is authorized.
    pub async fn authorized(&self, client: &ClientIdentity, action: &str, reason: String) -> bool {
        let Ok((authorization, server)) = endpoints::<Authorization>() else {
            return false;
        };
        let connected = self
            .directory
            .connect(AUTHORIZATION.into(), None, server.into_handle())
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            return false;
        }
        let authorization = AuthorizationAsyncClient::new(authorization, self.reactor.clone());
        let answer = authorization
            .authorize(copy(client), action.into(), reason)
            .await;
        matches!(answer, Ok(Ok(a)) if a.allowed)
    }
}

/// A copy of `client`.
pub fn copy(client: &ClientIdentity) -> ClientIdentity {
    ClientIdentity {
        pid: client.pid,
        user: client.user,
        app_id: client.app_id.clone(),
    }
}
