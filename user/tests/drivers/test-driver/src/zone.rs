//! As the btrfs test driver: whether the time service sets the system
//! time zone on the root file system, once it has connected to it, and
//! whether the FAT server of the EFI system partition reads its local
//! times in the zone in effect then.

use crate::fat::{named, ok, open};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{NodeType, OpenFlags};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::time::{Clock, ClockAsyncClient};

const ZONE: &str = "Europe/Berlin";
/// Berlin's offsets from UTC, in winter and in summer.
const BERLIN: [i64; 2] = [3_600_000_000_000, 7_200_000_000_000];
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
/// is a link to its file, that a zone without a file is refused, and
/// what `in_effect` checks.
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
        Ok(Err(Status::NotFound)) => {}
        _ => return failed("refusing a zone without a file"),
    }
    in_effect(directory, &clock, reactor).await
}

/// Whether the FAT server of the EFI system partition reads the local
/// time of a stamp in the zone in effect when it reads it: a file
/// written in Berlin, read again once the zone is UTC, was modified one
/// or, in summer, two hours later than it was read in Berlin. The zone is
/// Berlin again afterwards.
async fn in_effect(
    directory: &DirectoryAsyncClient,
    clock: &ClockAsyncClient,
    reactor: &Reactor,
) -> Option<()> {
    let esp = named(directory, reactor, "esp").await?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    let Ok((file, _)) = open(&esp, "ZONE.TXT", create, reactor).await else {
        return failed("creating ZONE.TXT on the EFI system partition");
    };
    if ok(file.write(0, b"zone".to_vec()).await).is_none() {
        return failed("writing ZONE.TXT");
    }
    let modified = async || ok(file.get_attributes().await).map(|a| a.attributes.modified);
    let in_berlin = modified().await?;
    if !matches!(clock.set_timezone("UTC".into()).await, Ok(Ok(_))) {
        return failed("setting the time zone UTC");
    }
    let in_utc = modified().await?;
    if !matches!(clock.set_timezone(ZONE.into()).await, Ok(Ok(_))) {
        return failed("setting the time zone again");
    }
    ok(esp.unlink("ZONE.TXT".into(), false).await)?;
    match BERLIN.contains(&(in_utc - in_berlin)) {
        true => Some(()),
        false => failed("reading a time in the zone in effect"),
    }
}
