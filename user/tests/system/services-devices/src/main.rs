//! The device manager's names of devices and its listing of them, as a
//! system test, from a program that holds no permission of its own, as
//! selftest's tests do: it lists the devices, but cannot connect to one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::Managed;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{DeviceEntry, Devices, DevicesClient};
use mikros_rt::interfaces::mikros::service::Provider;
use mikros_test::*;

mikros_rt::managed_main!(main);

const FOREVER: i64 = i64::MAX;

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("names_and_order", names_and_order),
            ("class_and_pages", class_and_pages),
            ("names_registered", names_registered),
        ],
    )
}

/// A client of the device manager's `Devices`.
fn devices(s: &Own) -> Result<DevicesClient> {
    let (client, server) = endpoints::<Devices>().map_err(|_| Failure {
        what: "making a channel",
        status: 0,
    })?;
    let connected = s.services().connect(
        "mikros.driver.Devices".into(),
        None,
        server.into_handle(),
        FOREVER,
    );
    ensure(
        "connecting to the device list",
        matches!(connected, Ok(Ok(_))),
    )?;
    Ok(DevicesClient::new(client))
}

/// One page of `List`, and whether more follow.
fn list(
    devices: &DevicesClient,
    class: Option<&str>,
    after: Option<&str>,
) -> Result<(Vec<DeviceEntry>, bool)> {
    let listed = devices.list(class.map(String::from), after.map(String::from), FOREVER);
    let Ok(response) = listed else {
        return Err(Failure {
            what: "listing devices",
            status: 0,
        });
    };
    Ok((response.entries, response.more))
}

/// `<class>/<name>` of `entry`.
fn key(entry: &DeviceEntry) -> String {
    format!("{}/{}", entry.class, entry.name)
}

/// Every device has a name of letters and digits that starts with its
/// class, and a description; they come sorted by `<class>/<name>`, and
/// a disk is among them.
fn names_and_order(s: &Own) -> Result {
    let (entries, _) = list(&devices(s)?, None, None)?;
    ensure("listing some device", !entries.is_empty())?;
    for entry in &entries {
        let mut chars = entry.name.chars();
        let well_formed = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        ensure("a name of letters and digits", well_formed)?;
        ensure(
            "a name after its class",
            entry.name.starts_with(entry.class.as_str()),
        )?;
        ensure("a description", !entry.description.is_empty())?;
    }
    let keys: Vec<String> = entries.iter().map(key).collect();
    ensure("sorted names", keys.windows(2).all(|w| w[0] < w[1]))?;
    ensure("a disk", entries.iter().any(|e| e.class == "storage"))
}

/// A class gives only its devices, and `after` the ones after a name.
fn class_and_pages(s: &Own) -> Result {
    let devices = devices(s)?;
    let (all, _) = list(&devices, None, None)?;
    let (storage, _) = list(&devices, Some("storage"), None)?;
    let expected: Vec<String> = all
        .iter()
        .filter(|e| e.class == "storage")
        .map(key)
        .collect();
    let got: Vec<String> = storage.iter().map(key).collect();
    ensure("the disks of the class", got == expected)?;
    let first = key(&all[0]);
    let (rest, _) = list(&devices, None, Some(&first))?;
    let after: Vec<String> = all[1..].iter().map(key).collect();
    let got: Vec<String> = rest.iter().map(key).collect();
    ensure("the devices after the first", got == after)
}

/// A device's name is an instance of its class's protocol, which needs
/// the class's permission: without it, connecting is refused, where an
/// unknown name is not found.
fn names_registered(s: &Own) -> Result {
    let connect = |protocol: &str, instance: &str| {
        let (_, server) = endpoints::<Provider>().map_err(|_| Failure {
            what: "making a channel",
            status: 0,
        })?;
        let connected = s.services().connect(
            protocol.into(),
            Some(instance.into()),
            server.into_handle(),
            FOREVER,
        );
        match connected {
            Ok(Ok(_)) => Ok(Status::Ok),
            Ok(Err(status)) => Ok(status),
            Err(_) => Err(Failure {
                what: "asking the directory",
                status: 0,
            }),
        }
    };
    ensure(
        "refusing the first disk's name",
        connect("device.storage", "storage0")? == Status::AccessDenied,
    )?;
    ensure(
        "not finding an unknown name",
        connect("device.storage", "storage999")? == Status::NotFound,
    )
}
