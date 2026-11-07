//! Supervising drivers: starting the driver bound to a device, and
//! handling a `Start` that fails and a driver process that terminates
//! without being stopped.

use crate::devices::Devices;
use crate::start::{Started, Starter};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_manifest::Driver;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::sys;

/// A driver is restarted at most this many times within `WINDOW`, as a
/// service is.
const RESTARTS_MAX: usize = 5;
const WINDOW: i64 = 60_000_000_000;
const TERMINATED: u32 = 1 << 4;
const INFO_PROCESS: u32 = 2;
const INFO_PROCESS_SIZE: usize = 0x28;

/// What the supervising tasks share: the devices, the drivers of the
/// valid manifests, and what starting them takes.
pub struct Manager {
    pub devices: RefCell<Devices>,
    pub drivers: Vec<Driver>,
    pub starter: Starter,
    pub reactor: Reactor,
}

/// Starts the driver bound to device `id`, and handles its end.
///
/// When `Start` fails with a status, the driver declined the device: its
/// process is terminated and the device fails. When the `Driver` channel
/// fails before `Start` replies, the process is terminated, and that
/// counts as a termination. Each time the process terminates, it is
/// logged and the driver is started again, after the devices below this
/// one are removed and, for a device with a parent, the parent's driver
/// reset it with `ResetChild`, which may fail with `NOT_SUPPORTED`. After
/// `RESTARTS_MAX` restarts within `WINDOW`, or when `ResetChild` fails
/// otherwise, the device fails.
pub async fn supervise(m: Rc<Manager>, id: u64) {
    let mut restarts = VecDeque::new();
    loop {
        let Some(driver) = bound_driver(&m, id) else {
            return;
        };
        let name = &driver.name;
        let prepared = match m.devices.borrow().get(id) {
            Some(device) => m.starter.prepare(device, driver),
            None => return,
        };
        let spawned = match prepared {
            Ok(prepared) => m.starter.start(prepared, driver).await,
            Err(why) => Err(why),
        };
        let spawned = match spawned {
            Ok(spawned) => spawned,
            Err(why) => {
                mikros_rt::log!(
                    Error,
                    "device-manager: driver {name} for device {id} did not start: {why}"
                );
                return;
            }
        };
        let pid = spawned.pid;
        match spawned.started {
            Started::Yes => {
                mikros_rt::log!(
                    Info,
                    "device-manager: started driver {name} for device {id} as pid {pid}"
                );
                if let Some(device) = m.devices.borrow_mut().get_mut(id) {
                    device.client = Some(spawned.driver.clone());
                }
            }
            Started::Declined(status) => {
                mikros_rt::log!(
                    Error,
                    "device-manager: driver {name} declined device {id}: {status:?}"
                );
                m.starter.kill(pid).await;
                m.devices.borrow_mut().fail(id);
                return;
            }
            Started::Closed => {
                mikros_rt::log!(
                    Error,
                    "device-manager: driver {name} for device {id} closed its channel before Start replied"
                );
                m.starter.kill(pid).await;
            }
        }
        let ended = exited(&m.reactor, &spawned.process).await;
        if let Some(device) = m.devices.borrow_mut().get_mut(id) {
            device.client = None;
        }
        match ended {
            Some((code, signal)) => mikros_rt::log!(
                Error,
                "device-manager: driver {name} for device {id} terminated with code {code}, signal {signal}"
            ),
            None => mikros_rt::log!(
                Error,
                "device-manager: driver {name} for device {id} terminated"
            ),
        }
        if !may_restart(&mut restarts) {
            mikros_rt::log!(
                Error,
                "device-manager: driver {name} for device {id} failed after {RESTARTS_MAX} restarts within 60 s"
            );
            m.devices.borrow_mut().fail(id);
            return;
        }
        m.devices.borrow_mut().remove_descendants(id);
        if !reset(&m, id).await {
            m.devices.borrow_mut().fail(id);
            return;
        }
        mikros_rt::log!(
            Info,
            "device-manager: restarting driver {name} for device {id}"
        );
    }
}

/// The driver bound to device `id`, if it has one.
fn bound_driver(m: &Manager, id: u64) -> Option<&Driver> {
    let devices = m.devices.borrow();
    let name = devices.get(id)?.driver.as_ref()?;
    m.drivers.iter().find(|d| &d.name == name)
}

/// Whether a driver that `restarts` holds the times of earlier restarts
/// for may be restarted now, which is then recorded.
fn may_restart(restarts: &mut VecDeque<i64>) -> bool {
    let now = now();
    while restarts.front().is_some_and(|&t| now - t >= WINDOW) {
        restarts.pop_front();
    }
    if restarts.len() >= RESTARTS_MAX {
        return false;
    }
    restarts.push_back(now);
    true
}

/// Has the driver of the parent of device `id` reset it, when it has a
/// parent. Returns whether the device may be started again: when it is
/// a root device, or `ResetChild` succeeded or failed with
/// `NOT_SUPPORTED`.
async fn reset(m: &Manager, id: u64) -> bool {
    let parent = {
        let devices = m.devices.borrow();
        let Some(parent) = devices.get(id).and_then(|d| d.parent) else {
            return true;
        };
        devices.get(parent).and_then(|p| p.client.clone())
    };
    let reset = match parent {
        Some(parent) => parent.reset_child(id).await,
        None => {
            mikros_rt::log!(
                Error,
                "device-manager: the parent of device {id} has no driver to reset it"
            );
            return false;
        }
    };
    match reset {
        Ok(Ok(_)) | Ok(Err(Status::NotSupported)) => true,
        failed => {
            mikros_rt::log!(
                Error,
                "device-manager: ResetChild for device {id} failed: {:?}",
                failed.map(|r| r.err())
            );
            false
        }
    }
}

/// Waits for `process` to end, and returns its exit code and the signal
/// that ended it, or `None` when they cannot be read.
async fn exited(reactor: &Reactor, process: &OwnedHandle) -> Option<(i64, u32)> {
    reactor.signals(process.raw(), TERMINATED).await.ok()?;
    let mut record = [0u8; INFO_PROCESS_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            process.raw(),
            INFO_PROCESS,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK {
        return None;
    }
    let code = i64::from_le_bytes(record[..8].try_into().ok()?);
    let signal = u32::from_le_bytes(record[0x10..0x14].try_into().ok()?);
    Some((code, signal))
}

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
