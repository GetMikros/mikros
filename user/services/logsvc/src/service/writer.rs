//! `mikros.log.Writer`: the records that a client writes, with the pid
//! and app ID of the client's identity.

use crate::service::log::{Log, severity_value};
use alloc::rc::Rc;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::log::{Writer, WriterAsyncServer, WriterRequest};
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// Adds what the client `client` writes on `end` to `log` until it
/// closes the connection.
pub async fn serve(log: Rc<Log>, end: ServerEnd<Writer>, reactor: Reactor, client: ClientIdentity) {
    let server = WriterAsyncServer::new(end, reactor);
    while let Ok(WriterRequest::Write(write)) = server.next().await {
        let severity = severity_value(write.severity);
        log.add_copied(client.pid, client.app_id.clone(), severity, write.message);
    }
}
