//! `mikros.log.OwnReader`: what `Reader.Read` gives, without a
//! permission, but only the records of the client's app and of the
//! packages that the install service lists as owned by it, its
//! components (MIKSPEC-0063.R19).

use crate::service::log::Log;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::log::{
    OwnReader, OwnReaderAsyncServer, OwnReaderReadResponse, OwnReaderRequest,
};
use mikros_rt::interfaces::mikros::packages::{Installer, InstallerAsyncClient};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, DirectoryAsyncClient};

const INSTALLER: &str = "mikros.packages.Installer";
/// The most records one reply holds.
const BATCH: usize = 32;

/// Serves `client`'s reader on `end` until it closes the connection.
pub async fn serve(
    log: Rc<Log>,
    end: ServerEnd<OwnReader>,
    reactor: Reactor,
    client: ClientIdentity,
    directory: Rc<DirectoryAsyncClient>,
) {
    let server = OwnReaderAsyncServer::new(end, reactor.clone());
    while let Ok(OwnReaderRequest::Read(read, responder)) = server.next().await {
        let owned = owned(&directory, &reactor, &client.app_id).await;
        let max = usize::try_from(read.max).unwrap_or(BATCH).min(BATCH);
        let records = log
            .after(read.after, max)
            .into_iter()
            .filter(|r| r.app_id == client.app_id || owned.contains(&r.app_id))
            .collect();
        let _ = responder.reply(server.server(), OwnReaderReadResponse { records });
    }
}

/// The IDs of the packages whose owner the install service lists as
/// `app_id`; none without an install service.
async fn owned(directory: &DirectoryAsyncClient, reactor: &Reactor, app_id: &str) -> Vec<String> {
    let Ok((client, server)) = endpoints::<Installer>() else {
        return Vec::new();
    };
    let connected = directory
        .connect(INSTALLER.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return Vec::new();
    }
    let installer = InstallerAsyncClient::new(client, reactor.clone());
    let mut owned = Vec::new();
    let mut after = String::new();
    loop {
        let page = match installer.list(after.clone()).await {
            Ok(Ok(r)) => r.packages,
            _ => return owned,
        };
        let Some(last) = page.last().map(|p| p.id.clone()) else {
            return owned;
        };
        owned.extend(page.into_iter().filter(|p| p.owner == app_id).map(|p| p.id));
        after = last;
    }
}
