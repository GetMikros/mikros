//! Whether a client holds a permission, which the permission broker
//! decides, and whether the broker allows a client an `admin` permission
//! when asked, which an administrator may allow.

use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, DirectoryAsyncClient};

const BROKER: &str = "mikros.permissions.Broker";

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

    /// The service's directory.
    pub fn directory(&self) -> &DirectoryAsyncClient {
        &self.directory
    }

    /// Whether the broker allows `client` `permission`. Without an
    /// answer, it does not.
    pub async fn holds(&self, client: &ClientIdentity, permission: &str) -> bool {
        let Some(broker) = self.broker().await else {
            return false;
        };
        match broker.check(client.clone(), permission.into()).await {
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

    /// Whether the broker allows `client` `permission` when asked with
    /// `Request` for `reason`. Without an answer, it does not.
    pub async fn requested(
        &self,
        client: &ClientIdentity,
        permission: &str,
        reason: String,
    ) -> bool {
        let Some(broker) = self.broker().await else {
            return false;
        };
        match broker
            .request(client.clone(), permission.into(), reason)
            .await
        {
            Ok(Ok(requested)) => requested.decision == Decision::Allowed,
            Ok(Err(_)) => false,
            Err(_) => {
                *self.broker.borrow_mut() = None;
                false
            }
        }
    }
}
