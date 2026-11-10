//! Stopping the device manager: on `TERM`, which the service manager
//! sends to stop it, every device but the ACPI device is removed,
//! children before their parents, the ACPI server prepares for power-off
//! and restart, and the device manager then exits.

use crate::supervise::{self, EJECT_STOP_LIMIT, Manager};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_rt::sys;

/// The mask bit of `TERM`, signal 15.
const TERM: u64 = 1 << (15 - 1);
/// How often the device manager looks for a pending `TERM`.
const POLL: i64 = 100_000_000;
/// The hardware ID of the device whose driver, the ACPI server, keeps
/// running.
const ACPI: &str = "platform:acpi";
/// How long the ACPI server has to prepare for power-off and restart.
const PREPARE_LIMIT: i64 = 60_000_000_000;

/// Blocks `TERM` for the calling thread, the device manager's only one,
/// so that it stays pending for `watch` instead of terminating the
/// process.
pub fn block_term() {
    // SAFETY: the call takes no pointers but `old`, which is null.
    unsafe { sys::signal_set_mask(0, TERM, core::ptr::null_mut()) };
}

/// Takes a pending `TERM`, if any, and returns whether there was one.
fn take_term() -> bool {
    let mut info = [0u8; 64];
    // SAFETY: the pointer refers to room for the signal's information.
    unsafe { sys::signal_wait(TERM, 0, info.as_mut_ptr()) == mikros_rt::OK }
}

/// Waits for `TERM`, looking once every `POLL`, then removes every
/// device: each root device with all the devices below it, children
/// before their parents, giving each driver `EJECT_STOP_LIMIT` after
/// `Stop` before it is terminated. Of a `platform:acpi` device, only the
/// devices below it are removed: it and its driver, the ACPI server,
/// stay, so that the machine can still be powered off or restarted
/// through it. The ACPI server is then called `PrepareShutdown`, and has
/// `PREPARE_LIMIT` to reply, while the device manager still grants its
/// resources. Then the device manager exits.
pub async fn watch(m: Rc<Manager>) {
    while !take_term() {
        let _ = m
            .reactor
            .sleep_until(supervise::now().saturating_add(POLL))
            .await;
    }
    mikros_rt::log!(Info, "device-manager: stopping, removing every device");
    let roots: Vec<(u64, bool)> = m
        .devices
        .borrow()
        .iter()
        .filter(|d| d.parent.is_none())
        .map(|d| (d.id, d.hardware_ids.iter().any(|id| id == ACPI)))
        .collect();
    for (root, acpi) in roots {
        if !acpi {
            supervise::remove_within(m.clone(), root, EJECT_STOP_LIMIT).await;
            continue;
        }
        let children = m.devices.borrow().children(root);
        for child in children {
            supervise::remove_within(m.clone(), child, EJECT_STOP_LIMIT).await;
        }
    }
    prepare_acpi(&m).await;
    mikros_rt::log!(
        Info,
        "device-manager: every device but the ACPI device removed, exiting"
    );
    // SAFETY: the call takes no pointers.
    unsafe { sys::process_exit(0) };
}

/// Calls `PrepareShutdown` on the driver of the ACPI device, if it has
/// one, and waits for the reply for at most `PREPARE_LIMIT`, serving the
/// driver's requests meanwhile, as the device manager's other tasks do.
async fn prepare_acpi(m: &Manager) {
    let client = m
        .devices
        .borrow()
        .iter()
        .find(|d| d.hardware_ids.iter().any(|id| id == ACPI))
        .and_then(|d| d.client.clone());
    let Some(client) = client else {
        return;
    };
    let deadline = supervise::now().saturating_add(PREPARE_LIMIT);
    let (mut call, mut limit) = (
        pin!(client.prepare_shutdown()),
        pin!(m.reactor.sleep_until(deadline)),
    );
    let reply = poll_fn(|cx| {
        if let Poll::Ready(reply) = call.as_mut().poll(cx) {
            return Poll::Ready(Some(reply));
        }
        match limit.as_mut().poll(cx) {
            Poll::Ready(_) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    match reply {
        Some(Ok(Ok(_))) => {
            mikros_rt::log!(
                Info,
                "device-manager: the ACPI server prepared for shutdown"
            );
        }
        Some(Ok(Err(status))) => mikros_rt::log!(
            Error,
            "device-manager: the ACPI server failed to prepare for shutdown: {status:?}"
        ),
        Some(Err(e)) => mikros_rt::log!(
            Error,
            "device-manager: the ACPI server failed to prepare for shutdown: {e:?}"
        ),
        None => mikros_rt::log!(
            Error,
            "device-manager: the ACPI server did not prepare for shutdown within 60 s"
        ),
    }
}
