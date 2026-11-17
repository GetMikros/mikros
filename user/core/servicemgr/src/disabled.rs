//! The disabled services (MIKSPEC-0036.R49): their names, one per line, in
//! the file `disabled` of the service manager's `/system/state/servicemgr`,
//! written and synced before `Enable` and `Disable` reply. Names without a
//! valid manifest stay, so that a package installed again stays disabled.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::files;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeAsyncClient, OpenFlags, Rights};

/// The file of the disabled services.
const FILE: &str = "disabled";
/// The longest file that is read.
const FILE_MAX: usize = 1 << 16;

/// The names in the text `text`, one per line, without empty lines.
fn parse(text: &[u8]) -> BTreeSet<String> {
    String::from_utf8_lossy(text)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// The disabled services that `state` keeps; none when it keeps no file.
pub async fn read(state: &NodeAsyncClient, reactor: &Reactor) -> BTreeSet<String> {
    match files::read(state, reactor, FILE, FILE_MAX).await {
        Ok(bytes) => parse(&bytes),
        Err(_) => BTreeSet::new(),
    }
}

/// Writes `disabled` as the disabled services that `state` keeps, and
/// syncs the file.
pub async fn write(
    state: &NodeAsyncClient,
    reactor: &Reactor,
    disabled: &BTreeSet<String>,
) -> Result<(), Status> {
    let mut text = Vec::new();
    for name in disabled {
        text.extend_from_slice(name.as_bytes());
        text.push(b'\n');
    }
    let (file, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    match state.open(FILE.into(), flags, 0o644, rw, server).await {
        Ok(Ok(_)) => {}
        _ => return Err(Status::Io),
    }
    let file = NodeAsyncClient::new(file, reactor.clone());
    if !text.is_empty() {
        match file.write(0, text).await {
            Ok(Ok(_)) => {}
            _ => return Err(Status::Io),
        }
    }
    match file.sync().await {
        Ok(Ok(_)) => Ok(()),
        _ => Err(Status::Io),
    }
}
