//! The boot attempts service, as a system test, from the side of a program
//! that holds no permission of its own, as selftest's tests do: it cannot
//! connect to it.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::Managed;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::boot::Attempts;
use mikros_test::*;

mikros_rt::managed_main!(main);

const FOREVER: i64 = i64::MAX;

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[(
            "attempts_need_their_permission",
            attempts_need_their_permission,
        )],
    )
}

/// `mikros.boot.Attempts` is refused without `boot.confirm`.
fn attempts_need_their_permission(s: &Own) -> Result {
    let (_client, server) = endpoints::<Attempts>().map_err(|_| Failure {
        what: "making a channel",
        status: 0,
    })?;
    let connected = s.services().connect(
        "mikros.boot.Attempts".into(),
        None,
        server.into_handle(),
        FOREVER,
    );
    ensure(
        "refusing the connection",
        matches!(connected, Ok(Err(Status::AccessDenied))),
    )
}
