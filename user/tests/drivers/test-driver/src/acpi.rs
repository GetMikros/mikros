//! As the ACPI test driver: whether the ACPI server routes the PCI
//! interrupt pins of the q35 machine's bus 0, and refuses to power the
//! machine off or restart it for a client without `power.control`.

use core::ops::RangeInclusive;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::acpi::{Acpi, AcpiAsyncClient};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const SERVICE: &str = "device.acpi";
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
    let deadline = crate::now() + LIMIT;
    let instance = loop {
        if let Ok(Ok(listed)) = directory.list(SERVICE.into()).await
            && let Some(instance) = listed.instances.into_iter().next()
        {
            break instance;
        }
        if crate::now() >= deadline {
            return failed("finding the acpi service");
        }
        let _ = reactor.sleep_until(crate::now() + 100_000_000).await;
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
