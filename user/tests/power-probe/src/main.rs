//! The boot service of `tests/power.img`, which the power-off run of
//! `cargo xtask test` boots: it connects to the ACPI server's
//! `device.acpi`, waits for a reply to one call on it, connects to the
//! service manager's `mikros.service.Control`, logs that it waits for
//! the power button, and once the ACPI server
//! sends `OnPowerButton`, which the runner raises through QEMU's monitor,
//! asks the service manager to shut the machine down and power it off.
//! It logs what failed, and exits with 1, when something does.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::acpi::{Acpi, AcpiAsyncClient, AcpiEvent};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{
    Control, ControlAsyncClient, DirectoryAsyncClient, ShutdownKind,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the `log_manage` boot handle among the `USER` startup
/// handles.
const LOG_ID: u32 = 2;
const ACPI: &str = "device.acpi";
const CONTROL: &str = "mikros.service.Control";
/// How long the ACPI server may take to publish its service, the
/// processor emulated.
const LIMIT: i64 = 120_000_000_000;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn failed(what: &str) -> i64 {
    mikros_rt::log!(Error, "power-probe: {what} failed");
    1
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let log = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID);
    if let Some(log) = log {
        mikros_rt::log::set_handle(log.handle.raw());
    }
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    executor.block_on(power_off(&directory, &reactor))
}

async fn power_off(directory: &DirectoryAsyncClient, reactor: &Reactor) -> i64 {
    let deadline = now() + LIMIT;
    let instance = loop {
        if let Ok(Ok(listed)) = directory.list(ACPI.into()).await
            && let Some(instance) = listed.instances.into_iter().next()
        {
            break instance;
        }
        if now() >= deadline {
            return failed("finding the acpi service");
        }
        let _ = reactor.sleep_until(now() + 100_000_000).await;
    };
    let Ok((client, server)) = endpoints::<Acpi>() else {
        return 1;
    };
    let connected = directory
        .connect(ACPI.into(), Some(instance), server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the acpi service");
    }
    let acpi = AcpiAsyncClient::new(client, reactor.clone());
    // A reply, whatever it says, means that the server serves the
    // connection, and so sends it the power button's events.
    if acpi.get_pci_route(0, 0, 0, 1).await.is_err() {
        return failed("calling the acpi service");
    }
    let Ok((client, server)) = endpoints::<Control>() else {
        return 1;
    };
    let connected = directory
        .connect(CONTROL.into(), None, server.into_handle())
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return failed("connecting to the service manager's Control");
    }
    let control = ControlAsyncClient::new(client, reactor.clone());
    mikros_rt::log!(Info, "power-probe: waiting for the power button");
    match acpi.next_event().await {
        Ok(AcpiEvent::OnPowerButton(_)) => {}
        _ => return failed("waiting for the power button"),
    }
    mikros_rt::log!(Info, "power-probe: the power button was pressed");
    // The service manager stops this service before it powers the
    // machine off; a reply means that it did not.
    let shut_down = control.shutdown(ShutdownKind::PowerOff).await;
    mikros_rt::log!(Error, "power-probe: Shutdown returned {shut_down:?}");
    1
}
