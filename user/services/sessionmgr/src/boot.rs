//! Marking the boot good once the text console has started, through the
//! boot attempts service, so that the bootloader keeps the version.

use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::boot::{Attempts, AttemptsAsyncClient};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const ATTEMPTS: &str = "mikros.boot.Attempts";

/// Calls `MarkGood` of the boot attempts service, reached through
/// `directory`, and logs a failure.
pub async fn mark_good(directory: &DirectoryAsyncClient, reactor: &Reactor) {
    let Ok((client, server)) = endpoints::<Attempts>() else {
        mikros_rt::log!(
            Error,
            "sessionmgr: marking the boot good failed: no channel"
        );
        return;
    };
    let connected = directory
        .connect(ATTEMPTS.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        mikros_rt::log!(
            Error,
            "sessionmgr: marking the boot good failed: no boot attempts service: {connected:?}"
        );
        return;
    }
    let attempts = AttemptsAsyncClient::new(client, reactor.clone());
    match attempts.mark_good().await {
        Ok(Ok(_)) => mikros_rt::log!(Info, "sessionmgr: marked the boot good"),
        other => mikros_rt::log!(Error, "sessionmgr: marking the boot good failed: {other:?}"),
    }
}
