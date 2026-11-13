//! The time service, as a system test, from the side of a program that
//! holds no permission of its own, as selftest's tests do: it cannot set
//! the time or the zone.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::Managed;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::time::{Clock, ClockClient};
use mikros_test::*;

mikros_rt::managed_main!(main);

const FOREVER: i64 = i64::MAX;

fn main(m: &Managed) -> i64 {
    mikros_test::run(m, &[("setting_refused", setting_refused)])
}

/// A client of the time service's `Clock`.
fn clock(s: &Own) -> Result<ClockClient> {
    let (client, server) = endpoints::<Clock>().map_err(|_| Failure {
        what: "making a channel",
        status: 0,
    })?;
    let connected = s.services().connect(
        "mikros.time.Clock".into(),
        None,
        server.into_handle(),
        FOREVER,
    );
    ensure("connecting to the clock", matches!(connected, Ok(Ok(_))))?;
    Ok(ClockClient::new(client))
}

/// Without `time.set`, the time service refuses to set UTC or the zone.
fn setting_refused(s: &Own) -> Result {
    let clock = clock(s)?;
    ensure(
        "refusing to set UTC",
        matches!(clock.set_utc(0, FOREVER), Ok(Err(Status::AccessDenied))),
    )?;
    ensure(
        "refusing to set the zone",
        matches!(
            clock.set_timezone("UTC".into(), FOREVER),
            Ok(Err(Status::AccessDenied))
        ),
    )
}
