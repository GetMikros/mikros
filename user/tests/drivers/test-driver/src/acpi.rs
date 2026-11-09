//! As the ACPI test driver: whether the ACPI server routes the PCI
//! interrupt pins of the q35 machine's bus 0, and refuses to power the
//! machine off or restart it for a client without `power.control`; and
//! whether the RTC driver reads and sets the clock that the server adds.

use alloc::string::String;
use core::ops::{Range, RangeInclusive};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::acpi::{Acpi, AcpiAsyncClient};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::time::{Rtc, RtcAsyncClient};

const SERVICE: &str = "device.acpi";
const RTC: &str = "device.rtc";
const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// 2024-01-01 to 2100-01-01: the clock's time lies within them.
const PLAUSIBLE: Range<i64> = 1_704_067_200 * NANOS_PER_SECOND..4_102_444_800 * NANOS_PER_SECOND;
const DAY: i64 = 86_400 * NANOS_PER_SECOND;
/// 1999-06-01, which the clock holds only with its century register.
const YEAR_1999: i64 = 928_195_200 * NANOS_PER_SECOND;
/// How long the ACPI server may take to start ACPICA and publish its
/// service, the processor emulated.
const LIMIT: i64 = 120_000_000_000;
/// The GSIs of q35's PCI interrupt links, GSIA to GSIH, in APIC mode.
const LINKS: RangeInclusive<u32> = 16..=23;

fn failed(what: &str) -> bool {
    mikros_rt::log!(Error, "test-acpi: {what} failed");
    false
}

/// Connects to the `acpi` service once the server publishes it, and
/// checks that pin D of slot 1 of bus 0, the e1000e function's, and pin
/// A of slots 2 and 0x1f, the virtio disk's and the LPC bridge's, are
/// routed through a link, level-triggered, with the polarity that QEMU's
/// links give, active high; that pin 0 is refused with `INVALID_ARGS`;
/// that bus 0x80, which no bridge reaches, has no route; and that
/// `PowerOff` and `Restart` fail with `ACCESS_DENIED`.
pub async fn works(directory: &DirectoryAsyncClient, reactor: &Reactor) -> bool {
    let Some(instance) = instance(directory, reactor, SERVICE).await else {
        return failed("finding the acpi service");
    };
    let Ok((client, server)) = endpoints::<Acpi>() else {
        return false;
    };
    let connected = directory
        .connect(SERVICE.into(), Some(instance), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the acpi service");
    }
    let acpi = AcpiAsyncClient::new(client, reactor.clone());
    for (device, pin) in [(1, 4), (2, 1), (0x1f, 1)] {
        match acpi.get_pci_route(0, 0, device, pin).await {
            Ok(Ok(r)) if LINKS.contains(&r.route.gsi) && r.route.level => {}
            _ => return failed("routing a pin of bus 0"),
        }
    }
    if !matches!(
        acpi.get_pci_route(0, 0, 2, 0).await,
        Ok(Err(Status::InvalidArgs))
    ) {
        return failed("refusing pin 0");
    }
    if !matches!(
        acpi.get_pci_route(0, 0x80, 0, 1).await,
        Ok(Err(Status::NotFound))
    ) {
        return failed("finding no route on bus 0x80");
    }
    if !matches!(acpi.power_off().await, Ok(Err(Status::AccessDenied))) {
        return failed("refusing PowerOff");
    }
    if !matches!(acpi.restart().await, Ok(Err(Status::AccessDenied))) {
        return failed("refusing Restart");
    }
    true
}

/// The first instance of `service` once one is published, within
/// `LIMIT`.
async fn instance(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    service: &str,
) -> Option<String> {
    let deadline = crate::now() + LIMIT;
    loop {
        if let Ok(Ok(listed)) = directory.list(service.into()).await
            && let Some(instance) = listed.instances.into_iter().next()
        {
            return Some(instance);
        }
        if crate::now() >= deadline {
            return None;
        }
        let _ = reactor.sleep_until(crate::now() + 100_000_000).await;
    }
}

/// Connects to the `rtc` service once the RTC driver publishes it, and
/// checks that the clock reads a time between 2024 and 2100, and that a
/// time a day later, and one in 1999, which needs the century register,
/// each read back once set. The clock is set back to its time at the end.
pub async fn rtc_works(directory: &DirectoryAsyncClient, reactor: &Reactor) -> bool {
    let Some(instance) = instance(directory, reactor, RTC).await else {
        return failed("finding the rtc service");
    };
    let Ok((client, server)) = endpoints::<Rtc>() else {
        return false;
    };
    let connected = directory
        .connect(RTC.into(), Some(instance), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the rtc service");
    }
    let rtc = RtcAsyncClient::new(client, reactor.clone());
    let (read, at) = match rtc.get().await {
        Ok(Ok(read)) if PLAUSIBLE.contains(&read.utc) => (read.utc, crate::now()),
        _ => return failed("reading the clock"),
    };
    let mut works = true;
    for set in [read + DAY, YEAR_1999] {
        let back = match rtc.set(set).await {
            Ok(Ok(_)) => rtc.get().await,
            _ => {
                works = failed("setting the clock");
                break;
            }
        };
        if !matches!(back, Ok(Ok(ref b)) if (set..set + 5 * NANOS_PER_SECOND).contains(&b.utc)) {
            works = failed("reading back the time set");
            break;
        }
    }
    let _ = rtc.set(read + (crate::now() - at)).await;
    works
}
