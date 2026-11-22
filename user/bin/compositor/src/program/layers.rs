//! The permission for layer surfaces above the windows (MIKSPEC-0076.R25):
//! the server says which clients need to be asked, the compositor asks
//! the permission broker for each with `Request`, without waiting for the
//! answer in its other work, and tells the server what it answered; and
//! it watches the permission with `WatchConfirmed`, so that the server
//! closes the layer surfaces of a client that it is revoked for, before
//! `RevocationDone`.

use super::{Compositor, Importer};
use crate::server::ClientId;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::permissions::{
    Broker, BrokerAsyncClient, BrokerEvent, Decision,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use std::rc::Rc;

const BROKER: &str = "mikros.permissions.Broker";
/// The permission for the layers `top` and `overlay`.
const PERMISSION: &str = "user.layer_surfaces.use";

impl Compositor {
    /// A new connection to the permission broker.
    async fn connect_broker(&self) -> Option<BrokerAsyncClient> {
        let (client, server) = endpoints::<Broker>().ok()?;
        let connected = self
            .directory
            .connect(BROKER.into(), None, server.into_handle())
            .await;
        matches!(connected, Ok(Ok(_))).then(|| BrokerAsyncClient::new(client, self.reactor.clone()))
    }

    /// Starts asking for the permission of each client that the server
    /// has a question for.
    pub(super) fn ask_layer_questions(self: &Rc<Self>) {
        let questions = self.server.borrow_mut().take_layer_questions();
        for client in questions {
            let identity = self.identities.borrow().get(&client).cloned();
            let this = self.clone();
            self.spawner
                .spawn(async move { this.ask_layer_permission(client, identity).await });
        }
    }

    /// Asks the broker whether `client` may have layer surfaces above the
    /// windows, and tells the server the answer: a client that is not known
    /// or a failed request is a no.
    async fn ask_layer_permission(
        self: Rc<Self>,
        client: ClientId,
        identity: Option<ClientIdentity>,
    ) {
        let allowed = match (identity, self.connect_broker().await) {
            (Some(identity), Some(broker)) => {
                let asked = broker
                    .request(identity, PERMISSION.into(), String::new())
                    .await;
                matches!(asked, Ok(Ok(r)) if r.decision == Decision::Allowed)
            }
            _ => false,
        };
        {
            let mut store = self.store.borrow_mut();
            let mut importer = Importer {
                renderer: &self.renderer,
                store: &mut store,
            };
            self.server
                .borrow_mut()
                .answer_layer_question(&mut importer, client, allowed);
        }
        self.flush(client);
        self.changed.notify();
    }

    /// Watches the permission, and on each revocation closes the layer
    /// surfaces of the clients of that app and user above the windows and
    /// confirms, until the broker's connection closes.
    pub(super) async fn watch_layer_permission(self: Rc<Self>) {
        let Some(broker) = self.connect_broker().await else {
            mikros_rt::log!(
                Warning,
                "compositor: no permission broker to watch {PERMISSION}"
            );
            return;
        };
        if !matches!(broker.watch_confirmed(PERMISSION.into()).await, Ok(Ok(_))) {
            mikros_rt::log!(Error, "compositor: watching {PERMISSION} failed");
            return;
        }
        while let Ok(event) = broker.next_event().await {
            let BrokerEvent::OnRevokedConfirmed(e) = event else {
                continue;
            };
            let clients: Vec<ClientId> = self
                .identities
                .borrow()
                .iter()
                .filter(|(_, i)| i.user == e.user && i.app_id == e.app_id)
                .map(|(&c, _)| c)
                .collect();
            for client in clients {
                self.server.borrow_mut().revoke_layer_permission(client);
                self.flush(client);
            }
            self.changed.notify();
            let _ = broker.revocation_done(e.sequence).await;
        }
    }
}
