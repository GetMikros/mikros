//! The service manager's list of services, as a system test, from a
//! program that holds no permission of its own, as selftest's tests do.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::Managed;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::service::{
    ServiceEntry, ServiceState, Services, ServicesClient,
};
use mikros_test::*;

mikros_rt::managed_main!(main);

const FOREVER: i64 = i64::MAX;

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("lists_services", lists_services),
            ("lists_after_a_name", lists_after_a_name),
        ],
    )
}

/// A client of the service manager's `Services`.
fn services(s: &Own) -> Result<ServicesClient> {
    let (client, server) = endpoints::<Services>().map_err(|_| Failure {
        what: "making a channel",
        status: 0,
    })?;
    let connected = s.services().connect(
        "mikros.service.Services".into(),
        None,
        server.into_handle(),
        FOREVER,
    );
    ensure(
        "connecting to the list of services",
        matches!(connected, Ok(Ok(_))),
    )?;
    Ok(ServicesClient::new(client))
}

/// One page of `List`, and whether more follow.
fn list(services: &ServicesClient, after: Option<&str>) -> Result<(Vec<ServiceEntry>, bool)> {
    let Ok(response) = services.list(after.map(String::from), FOREVER) else {
        return Err(Failure {
            what: "listing services",
            status: 0,
        });
    };
    Ok((response.entries, response.more))
}

/// The services come sorted by name. The log service runs with a process
/// ID, and the terminal service starts on demand. A service that does
/// not run has no process ID.
fn lists_services(s: &Own) -> Result {
    let (entries, more) = list(&services(s)?, None)?;
    ensure("listing some service", !entries.is_empty() && !more)?;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    ensure("sorted names", names.windows(2).all(|w| w[0] < w[1]))?;
    let named = |name: &str| entries.iter().find(|e| e.name == name);
    let log = named("logsvc");
    ensure(
        "the log service running",
        log.is_some_and(|e| e.state == ServiceState::Running && e.pid != 0 && !e.on_demand),
    )?;
    ensure(
        "the terminal service on demand",
        named("termsvc").is_some_and(|e| e.on_demand),
    )?;
    let consistent = entries
        .iter()
        .all(|e| (e.state == ServiceState::Running) == (e.pid != 0));
    ensure("process IDs of the running services only", consistent)
}

/// `after` gives the services after a name.
fn lists_after_a_name(s: &Own) -> Result {
    let services = services(s)?;
    let (all, _) = list(&services, None)?;
    let (rest, _) = list(&services, Some(&all[0].name))?;
    let expected: Vec<&str> = all[1..].iter().map(|e| e.name.as_str()).collect();
    let got: Vec<&str> = rest.iter().map(|e| e.name.as_str()).collect();
    ensure("the services after the first", got == expected)?;
    let (none, more) = list(&services, Some("~"))?;
    ensure("none after the last name", none.is_empty() && !more)
}
