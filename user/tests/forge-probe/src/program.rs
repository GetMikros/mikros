//! The checks, on Mikros.

use mikros_command::Directory;
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerClient, Decision};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use std::os::mikros::process;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const BROKER: &str = "mikros.permissions.Broker";
const FOREVER: i64 = i64::MAX;
const PERMISSION: &str = "user.camera.use";
/// The app ID that the forged identity names instead of its own.
const FORGED: &str = "org.example.forged";
/// Much less than the 60 s that an open question waits for its answer.
const AT_ONCE: Duration = Duration::from_secs(10);

fn failed(code: u8, what: &str) -> ExitCode {
    eprintln!("forge-probe: {code}: {what}");
    ExitCode::from(code)
}

pub fn main() -> ExitCode {
    let Ok(me) = process::info(std::process::id()) else {
        return failed(1, "describing its own process");
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return failed(1, "no service directory");
    };
    let broker = match directory.connect::<Broker>(BROKER) {
        Ok(client) => BrokerClient::new(client),
        Err(why) => return failed(1, &why),
    };
    // Its own process, user, kernel object ID and checksum, but another
    // app's ID.
    let forged = ClientIdentity {
        pid: me.pid,
        user: me.user,
        app_id: FORGED.into(),
        admin: 0,
        koid: me.koid,
        portable_checksum: String::new(),
    };
    let start = Instant::now();
    let reason = "check that a forged client is not asked about".into();
    let requested = broker.request(forged.clone(), PERMISSION.into(), reason, FOREVER);
    if !matches!(&requested, Ok(Ok(r)) if r.decision == Decision::Denied) {
        return failed(
            2,
            &format!("Request for a forged client gave {requested:?}"),
        );
    }
    if start.elapsed() >= AT_ONCE {
        return failed(2, "the forged client was asked about");
    }
    let checked = broker.check(forged, PERMISSION.into(), FOREVER);
    if !matches!(&checked, Ok(Ok(r)) if r.decision == Decision::Undetermined) {
        return failed(3, &format!("a grant was recorded: {checked:?}"));
    }
    ExitCode::SUCCESS
}
