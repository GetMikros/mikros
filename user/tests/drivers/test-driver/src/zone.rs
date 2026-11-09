//! As the btrfs test driver: whether the time service sets the system
//! time zone on the root file system, once it has connected to it.

use crate::fat::{named, ok, open};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{NodeType, OpenFlags};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::time::{Clock, ClockAsyncClient};

const ZONE: &str = "Europe/Berlin";
/// How long the time service may take to connect to the root file
/// system, the processor emulated.
const LIMIT: i64 = 120_000_000_000;

fn failed(what: &str) -> Option<()> {
    mikros_rt::log!(Error, "test-btrfs: {what} failed");
    None
}

/// Sets the zone `Europe/Berlin` through the time service, asking again
/// while it says that the root file system is not available yet, and
/// checks that `GetTimezone` gives it, that `etc/localtime` on the root
/// is a link to its file, and that a zone without a file is refused.
pub async fn set_on_root(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<()> {
    let (client, server) = endpoints::<Clock>().ok()?;
    let connected = directory
        .connect("mikros.time.Clock".into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the time service");
    }
    let clock = ClockAsyncClient::new(client, reactor.clone());
    let deadline = crate::now() + LIMIT;
    loop {
        match clock.set_timezone(ZONE.into()).await {
            Ok(Ok(_)) => break,
            Ok(Err(Status::BadState)) if crate::now() < deadline => {
                let _ = reactor.sleep_until(crate::now() + 100_000_000).await;
            }
            _ => return failed("setting the time zone"),
        }
    }
    match clock.get_timezone().await {
        Ok(zone) if zone.name == ZONE => {}
        _ => return failed("reading the time zone"),
    }
    let root = named(directory, reactor, "root").await?;
    let Ok((link, kind)) = open(&root, "etc/localtime", OpenFlags::NO_FOLLOW, reactor).await else {
        return failed("opening etc/localtime");
    };
    let target = ok(link.read_link().await).map(|t| t.target);
    if kind != NodeType::Symlink || target.as_deref() != Some("/usr/share/zoneinfo/Europe/Berlin") {
        return failed("finding the link etc/localtime");
    }
    match clock.set_timezone("Nowhere/Atlantis".into()).await {
        Ok(Err(Status::NotFound)) => Some(()),
        _ => failed("refusing a zone without a file"),
    }
}
