//! Supervising drivers: starting the driver bound to a device, handling
//! a `Start` that fails and a driver process that terminates without
//! being stopped, and removing devices.

use crate::cache::Profiles;
use crate::devices::Devices;
use crate::publish::ServiceIds;
use crate::serve;
use crate::start::{Started, Starter};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::{Pin, pin};
use core::task::{Poll, Waker};
use mikros_manifest::{Driver, Need};
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::DeviceManager;
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, RegistrarAsyncClient};
use mikros_rt::sys;

/// A driver is restarted at most this many times within `WINDOW`, as a
/// service is.
const RESTARTS_MAX: usize = 5;
const WINDOW: i64 = 60_000_000_000;
const TERMINATED: u32 = 1 << 4;
const INFO_PROCESS: u32 = 2;
const INFO_PROCESS_SIZE: usize = 0x28;
/// How long a driver that is called `Stop` has before it is terminated,
/// and how long it has when its disk is ejected.
const STOP_LIMIT: i64 = 5_000_000_000;
pub const EJECT_STOP_LIMIT: i64 = 60_000_000_000;
/// How long after a restarted driver's `Start` the children it had
/// before are kept for it to add again.
const READD_LIMIT: i64 = 10_000_000_000;

/// What the supervising tasks share: the devices, the drivers of the
/// valid manifests, what starting them takes, the service manager's
/// `Registrar` and the service IDs given out, the service directory,
/// the storage profiles, and the executor's reactor and spawner.
pub struct Manager {
    pub devices: RefCell<Devices>,
    pub drivers: Vec<Driver>,
    pub starter: Starter,
    pub registrar: Option<RegistrarAsyncClient>,
    pub service_ids: ServiceIds,
    pub directory: DirectoryAsyncClient,
    pub profiles: Profiles,
    pub reactor: Reactor,
    pub spawner: Spawner,
}

/// Something that happens once, which tasks can wait for.
#[derive(Default)]
pub struct Done {
    done: Cell<bool>,
    waiters: RefCell<Vec<Waker>>,
}

impl Done {
    pub fn set(&self) {
        self.done.set(true);
        for waker in self.waiters.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    pub async fn wait(&self) {
        poll_fn(|cx| match self.done.get() {
            true => Poll::Ready(()),
            false => {
                self.waiters.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

/// Starts the driver bound to device `id`, and handles its end.
///
/// A device with a parent gets a new bus channel for each start: the
/// parent's driver is given one end with `ConnectChild`, and `Start`
/// the other. When `ConnectChild` fails, the device fails. The
/// `DeviceManager` channel that `Start` gives the driver is served from
/// before the call, and until the driver ends, when the services it
/// published are unregistered before it is started again.
///
/// When `Start` fails with a status, the driver declined the device: its
/// process is terminated and the device fails. When the `Driver` channel
/// fails before `Start` replies, the process is terminated, and that
/// counts as a termination. Each time the process terminates, unless the
/// device is being removed, it is logged and the driver is started
/// again, after, for a device with a parent, the parent's driver reset
/// it with `ResetChild`, which may fail with `NOT_SUPPORTED`. The
/// devices that the driver added are kept, with their drivers, for the
/// restarted driver to add again: those it has not added again
/// `READD_LIMIT` after its `Start` are removed. After `RESTARTS_MAX`
/// restarts within `WINDOW`, when `ResetChild` fails otherwise, or when
/// the driver declines the device, the device fails and its children
/// are removed.
pub async fn supervise(m: Rc<Manager>, id: u64) {
    let mut restarts = VecDeque::new();
    loop {
        let Some(driver) = bound_driver(&m, id) else {
            return;
        };
        let name = &driver.name;
        let (prepared, parent) = match m.devices.borrow().get(id) {
            Some(device) => (m.starter.prepare(device, driver), device.parent),
            None => return,
        };
        let did_not_start = |why: &str| {
            mikros_rt::log!(
                Error,
                "device-manager: driver {name} for device {id} did not start: {why}"
            );
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(why) => return did_not_start(&why),
        };
        let bus = match parent {
            None => None,
            Some(parent) => {
                let dma = driver.needs.contains(&Need::Dma);
                match connect_child(&m, parent, id, dma).await {
                    Ok(bus) => Some(bus),
                    Err(why) => {
                        did_not_start(&format!("ConnectChild failed: {why}"));
                        m.devices.borrow_mut().fail(id);
                        return;
                    }
                }
            }
        };
        let spawned = match m.starter.spawn(driver).await {
            Ok(spawned) => spawned,
            Err(why) => return did_not_start(&why),
        };
        let pid = spawned.pid;
        let removing = match m.devices.borrow_mut().get_mut(id) {
            Some(device) => {
                device.pid = Some(pid);
                device.client = Some(spawned.driver.clone());
                device.removing
            }
            None => true,
        };
        if removing {
            m.starter.kill(pid).await;
            return;
        }
        let Ok((manager, manager_server)) = endpoints::<DeviceManager>() else {
            m.starter.kill(pid).await;
            return did_not_start("creating its DeviceManager channel failed");
        };
        let served = Rc::new(Done::default());
        let serve = serve::serve(m.clone(), id, manager_server, served.clone());
        m.spawner.spawn(serve);
        let start = match m.devices.borrow_mut().get_mut(id) {
            Some(device) => {
                device.starts += 1;
                device.starts
            }
            None => 0,
        };
        let deadline = now().saturating_add(READD_LIMIT);
        m.spawner.spawn(forget_kept(m.clone(), id, start, deadline));
        let started = m
            .starter
            .start(&spawned.driver, prepared, bus, manager)
            .await;
        match started {
            Started::Yes => {
                mikros_rt::log!(
                    Info,
                    "device-manager: started driver {name} for device {id} as pid {pid}"
                );
            }
            Started::Declined(status) => {
                mikros_rt::log!(
                    Error,
                    "device-manager: driver {name} declined device {id}: {status:?}"
                );
                m.starter.kill(pid).await;
                m.devices.borrow_mut().fail(id);
                remove_children(&m, id).await;
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
        served.wait().await;
        let removing = match m.devices.borrow_mut().get_mut(id) {
            Some(device) => {
                device.pid = None;
                device.client = None;
                device.removing
            }
            None => true,
        };
        if removing {
            mikros_rt::log!(
                Info,
                "device-manager: stopped driver {name} for device {id}"
            );
            return;
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
            remove_children(&m, id).await;
            return;
        }
        {
            let mut devices = m.devices.borrow_mut();
            let next = devices.get(id).map_or(0, |d| d.starts + 1);
            devices.keep_children(id, next);
        }
        if !reset(&m, id).await {
            m.devices.borrow_mut().fail(id);
            remove_children(&m, id).await;
            return;
        }
        mikros_rt::log!(
            Info,
            "device-manager: restarting driver {name} for device {id}"
        );
    }
}

/// Removes the children of device `id`, each with the devices below it.
async fn remove_children(m: &Rc<Manager>, id: u64) {
    let children = m.devices.borrow().children(id);
    for child in children {
        remove(m.clone(), child).await;
    }
}

/// At `deadline`, removes the children of device `id` that are still
/// kept for its driver's start `start`, because that driver did not add
/// them again.
async fn forget_kept(m: Rc<Manager>, id: u64, start: u64, deadline: i64) {
    if m.devices.borrow().kept_children(id, start).is_empty() {
        return;
    }
    let _ = m.reactor.sleep_until(deadline).await;
    let kept = m.devices.borrow().kept_children(id, start);
    for child in kept {
        mikros_rt::log!(
            Info,
            "device-manager: device {child} was not added again in time"
        );
        remove(m.clone(), child).await;
    }
}

/// Reconnects device `id`, which its parent's restarted driver just
/// added again: has that driver serve its bus for it with
/// `ConnectChild`, and gives the other end to the device's own driver
/// with `Reconnect`. When either fails, the device's driver is
/// terminated, which its supervision handles as a driver that ended.
pub async fn reconnect(m: Rc<Manager>, id: u64) {
    let (parent, client, pid, dma) = {
        let devices = m.devices.borrow();
        let Some(device) = devices.get(id) else {
            return;
        };
        let dma = bound_driver(&m, id).is_some_and(|d| d.needs.contains(&Need::Dma));
        (device.parent, device.client.clone(), device.pid, dma)
    };
    let (Some(parent), Some(client)) = (parent, client) else {
        return;
    };
    let reconnected = match connect_child(&m, parent, id, dma).await {
        Ok(bus) => match client.reconnect(bus).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(status)) => Err(format!("{status:?}")),
            Err(e) => Err(format!("{e:?}")),
        },
        Err(why) => Err(format!("ConnectChild failed: {why}")),
    };
    match reconnected {
        Ok(()) => mikros_rt::log!(Info, "device-manager: reconnected device {id}"),
        Err(why) => {
            mikros_rt::log!(
                Error,
                "device-manager: reconnecting device {id} failed: {why}"
            );
            if let Some(pid) = pid {
                m.starter.kill(pid).await;
            }
        }
    }
}

/// Removes device `id`: first the devices below it, deepest first, then
/// the device itself. Its driver, when it has one, is called `Stop` and
/// terminated when `Stop` replies, when the channel fails, or after
/// `STOP_LIMIT`, and it is not started again; devices that it added
/// meanwhile are removed after it ended, before the device.
pub fn remove(m: Rc<Manager>, id: u64) -> Pin<Box<dyn Future<Output = ()>>> {
    remove_within(m, id, STOP_LIMIT)
}

/// Removes device `id` as `remove` does, but gives each driver `limit`
/// nanoseconds after `Stop` before it is terminated.
pub fn remove_within(m: Rc<Manager>, id: u64, limit: i64) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
        let children = m.devices.borrow().children(id);
        for child in children {
            remove_within(m.clone(), child, limit).await;
        }
        let (client, pid) = match m.devices.borrow_mut().get_mut(id) {
            Some(device) => {
                device.removing = true;
                (device.client.clone(), device.pid)
            }
            None => return,
        };
        if let Some(client) = client {
            let deadline = now().saturating_add(limit);
            first(client.stop(), m.reactor.sleep_until(deadline)).await;
        }
        if let Some(pid) = pid {
            m.starter.kill(pid).await;
        }
        // Devices that the driver added after the others were listed,
        // until it stopped.
        loop {
            let late = m.devices.borrow().children(id);
            if late.is_empty() {
                break;
            }
            for child in late {
                remove_within(m.clone(), child, limit).await;
            }
        }
        m.devices.borrow_mut().remove(id);
    })
}

/// Waits until `a` or `b` is done.
async fn first(a: impl Future, b: impl Future) {
    let (mut a, mut b) = (pin!(a), pin!(b));
    poll_fn(|cx| {
        let done = a.as_mut().poll(cx).is_ready() || b.as_mut().poll(cx).is_ready();
        if done { Poll::Ready(()) } else { Poll::Pending }
    })
    .await
}

/// Has the driver of device `parent` serve its bus for its child `id`
/// on a new channel, with `ConnectChild`, and returns the other end.
async fn connect_child(
    m: &Manager,
    parent: u64,
    id: u64,
    dma: bool,
) -> Result<OwnedHandle, String> {
    let client = m
        .devices
        .borrow()
        .get(parent)
        .and_then(|p| p.client.clone());
    let client = client.ok_or_else(|| String::from("its parent has no driver"))?;
    let (ours, theirs) = channel()?;
    match client.connect_child(id, theirs, dma).await {
        Ok(Ok(_)) => Ok(ours),
        Ok(Err(status)) => Err(format!("{status:?}")),
        Err(e) => Err(format!("{e:?}")),
    }
}

/// A new channel's two ends.
fn channel() -> Result<(OwnedHandle, OwnedHandle), String> {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    let status = unsafe { sys::channel_create(0, &mut a, &mut b) };
    if status != mikros_rt::OK {
        return Err(format!("creating a channel: {status}"));
    }
    // SAFETY: the kernel just created the handles for us.
    Ok(unsafe { (OwnedHandle::from_raw(a), OwnedHandle::from_raw(b)) })
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
    let (parent, client) = {
        let devices = m.devices.borrow();
        let Some(parent) = devices.get(id).and_then(|d| d.parent) else {
            return true;
        };
        (parent, devices.get(parent).and_then(|p| p.client.clone()))
    };
    let reset = match client {
        Some(client) => client.reset_child(id).await,
        None => {
            mikros_rt::log!(
                Error,
                "device-manager: the parent of device {id} has no driver to reset it"
            );
            return false;
        }
    };
    match reset {
        Ok(Ok(_)) => {
            mikros_rt::log!(
                Info,
                "device-manager: the driver of device {parent} reset device {id}"
            );
            true
        }
        Ok(Err(Status::NotSupported)) => {
            mikros_rt::log!(
                Info,
                "device-manager: the driver of device {parent} cannot reset device {id}"
            );
            true
        }
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

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}
