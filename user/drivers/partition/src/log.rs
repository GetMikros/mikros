//! Writing to the system log through the `mikros.log.Writer` that the
//! service directory connects to the log service. A message is lost when
//! there is no log service.

use alloc::string::String;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::log::{Severity, Writer, WriterAsyncClient};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

/// The protocol of the log service that writers use.
const WRITER: &str = "mikros.log.Writer";

/// Writes `message` with `severity`.
pub async fn write(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    severity: Severity,
    message: String,
) {
    let Ok((client, server)) = endpoints::<Writer>() else {
        return;
    };
    let connected = directory
        .connect(WRITER.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return;
    }
    let writer = WriterAsyncClient::new(client, reactor.clone());
    let _ = writer.write(severity, message);
}
