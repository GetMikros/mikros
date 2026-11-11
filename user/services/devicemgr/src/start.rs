//! Starting drivers: registering their permissions with the permission
//! broker, spawning each as a process of its own through the process
//! manager with its `Driver` channel as the `PROVIDER` startup handle,
//! and calling `Start` on that channel.

use crate::devices::Device;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_manifest::{Driver, Need};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceDescription, DeviceManager, Driver as DriverProtocol, DriverAsyncClient, Property,
};
use mikros_rt::interfaces::mikros::permissions::BrokerAsyncClient;
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::sys;

/// `user` in a Spawn request: the caller's own user.
const CALLER_USER: u32 = u32::MAX;
/// The signal that terminates a process.
const KILL: u32 = 9;
const SAME_RIGHTS: u32 = 1 << 31;
const PAGE_SIZE: u64 = 4096;
/// Resource kinds, and the `object_get_info` topic that tells them.
const KIND_IOPORT: u32 = 2;
const KIND_DMA: u32 = 4;
const KIND_PRIORITY: u32 = 5;
const SHARED: u32 = 1 << 0;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// The priorities that a driver's `priority` may reach beyond 16.
const PRIORITY_FIRST: i64 = 17;
const PRIORITY_LAST: i64 = 31;
/// The boot framebuffer's hardware ID, and the start of PCI display
/// controllers' class IDs.
const BOOT_FRAMEBUFFER: &str = "platform:boot-framebuffer";
const PCI_DISPLAY: &str = "pci:c03";
/// The `id` of the framebuffer's `USER` startup handle.
const FRAMEBUFFER_ID: u32 = 1;

/// Whether `hardware_id` is that of a display device.
fn is_display(hardware_id: &str) -> bool {
    hardware_id == BOOT_FRAMEBUFFER || hardware_id.starts_with(PCI_DISPLAY)
}

/// What starting drivers takes: the process manager's `Launcher`, the
/// broker, the root resource, and a `PRIORITY` resource covering 17 to
/// 31 when one could be made.
pub struct Starter {
    pub launcher: LauncherAsyncClient,
    pub broker: Option<BrokerAsyncClient>,
    pub root: OwnedHandle,
    pub priority: Option<OwnedHandle>,
    pub reactor: Reactor,
    /// The framebuffer's memory object, for the boot framebuffer's
    /// driver alone.
    framebuffer: Option<OwnedHandle>,
    /// The log handle with `MANAGE`, until the kernel console is
    /// released.
    console: RefCell<Option<OwnedHandle>>,
}

/// What `Start` gets for a device: its description and copies of the
/// resources its driver may have.
pub struct Prepared {
    description: DeviceDescription,
    resources: Vec<OwnedHandle>,
}

/// A driver that was spawned: its process ID and process, and the
/// client of its `Driver` channel.
pub struct Spawned {
    pub pid: u32,
    pub process: OwnedHandle,
    pub driver: Rc<DriverAsyncClient>,
}

/// How `Start` ended.
pub enum Started {
    Yes,
    /// It failed with this status.
    Declined(Status),
    /// The `Driver` channel failed before `Start` replied.
    Closed,
}

impl Starter {
    /// Creates the `PRIORITY` resource from `root`; without it, drivers
    /// get no priority above 16.
    pub fn new(
        launcher: LauncherAsyncClient,
        broker: Option<BrokerAsyncClient>,
        root: OwnedHandle,
        framebuffer: Option<OwnedHandle>,
        console: Option<OwnedHandle>,
        reactor: Reactor,
    ) -> Starter {
        let priority = resource(&root, KIND_PRIORITY, PRIORITY_FIRST as u64, 15, 0);
        if let Err(status) = &priority {
            mikros_rt::log!(
                Error,
                "device-manager: creating the PRIORITY resource failed: {status}"
            );
        }
        Starter {
            launcher,
            broker,
            root,
            priority: priority.ok(),
            reactor,
            framebuffer,
            console: RefCell::new(console),
        }
    }

    /// What `Start` gets for `device` when `driver` drives it: the
    /// device's ID, name, hardware IDs and properties, and its resources
    /// as `resources` chooses them.
    pub fn prepare(&self, device: &Device, driver: &Driver) -> Result<Prepared, String> {
        let description = description(device);
        let resources = self.resources(device, driver)?;
        Ok(Prepared {
            description,
            resources,
        })
    }

    /// Spawns `driver` for the device with `hardware_ids`, with app ID
    /// `system.driver.<name>`, after registering its permissions with
    /// the broker. Before the first driver of a display device, the
    /// kernel console is released; the boot framebuffer's driver gets
    /// the framebuffer's memory object.
    pub async fn spawn(&self, driver: &Driver, hardware_ids: &[String]) -> Result<Spawned, String> {
        if hardware_ids.iter().any(|id| is_display(id)) {
            self.release_console();
        }
        let app_id = format!("system.driver.{}", driver.name);
        let broker = self
            .broker
            .as_ref()
            .ok_or_else(|| String::from("there is no permission broker"))?;
        match broker
            .grant_system(app_id.clone(), driver.permissions.clone())
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(format!("registering its permissions: {status:?}")),
            Err(e) => return Err(format!("registering its permissions: {e:?}")),
        }
        let (client, server) = endpoints::<DriverProtocol>().map_err(|e| format!("{e:?}"))?;
        let mut handles = vec![StartupHandle {
            role: HandleRole::Provider,
            id: 0,
            handle: server.into_handle(),
        }];
        handles.extend(self.priority(driver.priority)?);
        if hardware_ids.iter().any(|id| id == BOOT_FRAMEBUFFER) {
            let framebuffer = self
                .framebuffer
                .as_ref()
                .ok_or_else(|| String::from("there is no framebuffer"))?;
            let framebuffer =
                duplicate(framebuffer).map_err(|s| format!("duplicating the framebuffer: {s}"))?;
            handles.push(StartupHandle {
                role: HandleRole::User,
                id: FRAMEBUFFER_ID,
                handle: framebuffer,
            });
        }
        let arguments = arguments(&driver.binary).map_err(|s| format!("arguments: {s}"))?;
        let request = SpawnRequest {
            path: driver.binary.clone(),
            arguments,
            handles,
            app_id: Some(app_id),
            user: CALLER_USER,
            login_session: false,
            new_session: false,
        };
        let spawned = match self.launcher.spawn(request).await {
            Ok(Ok(spawned)) => spawned,
            Ok(Err(status)) => return Err(format!("Spawn failed: {status:?}")),
            Err(e) => return Err(format!("Spawn failed: {e:?}")),
        };
        Ok(Spawned {
            pid: spawned.pid,
            process: spawned.process,
            driver: Rc::new(DriverAsyncClient::new(client, self.reactor.clone())),
        })
    }

    /// Releases the kernel console, once.
    fn release_console(&self) {
        let Some(log) = self.console.borrow_mut().take() else {
            return;
        };
        // SAFETY: the call takes no pointers.
        let status = unsafe { sys::log_console_release(log.raw()) };
        if status == mikros_rt::OK {
            mikros_rt::log!(Info, "device-manager: released the kernel console");
        } else {
            mikros_rt::log!(
                Error,
                "device-manager: releasing the kernel console failed: {status}"
            );
        }
    }

    /// Calls `Start` on `driver` with `prepared`, the bus channel `bus`
    /// of a device with a parent, and `manager`.
    pub async fn start(
        &self,
        driver: &DriverAsyncClient,
        prepared: Prepared,
        bus: Option<OwnedHandle>,
        manager: ClientEnd<DeviceManager>,
    ) -> Started {
        let started = driver
            .start(prepared.description, prepared.resources, bus, manager)
            .await;
        match started {
            Ok(Ok(_)) => Started::Yes,
            Ok(Err(status)) => Started::Declined(status),
            Err(_) => Started::Closed,
        }
    }

    /// A new resource of `kind` covering `size` from `base`, made from the
    /// root resource, with `SHARED` when `shared`, for `RequestResource`.
    pub fn request(
        &self,
        kind: u32,
        base: u64,
        size: u64,
        shared: bool,
    ) -> Result<OwnedHandle, Status> {
        let options = if shared { SHARED } else { 0 };
        resource(&self.root, kind, base, size, options).map_err(|s| Status::from_raw(s as i32))
    }

    /// Terminates the driver process `pid`, a child of the device
    /// manager, through the process manager. A process that already
    /// terminated, which `BAD_STATE` says, is left as it is.
    pub async fn kill(&self, pid: u32) {
        let killed = self.launcher.kill(pid, KILL).await;
        if let Ok(Err(status)) = killed
            && status != Status::BadState
        {
            mikros_rt::log!(
                Error,
                "device-manager: terminating driver pid {pid} failed: {status:?}"
            );
        }
    }

    /// The resources `driver` gets in `Start` for `device`: copies of the
    /// device's own, but its `IOPORT` resources only when the driver
    /// needs `ioports`, and a new `DMA` resource when it needs `dma`.
    fn resources(&self, device: &Device, driver: &Driver) -> Result<Vec<OwnedHandle>, String> {
        let needs = |need| driver.needs.contains(&need);
        let mut resources = Vec::new();
        for resource in &device.resources {
            let kind = kind(resource).map_err(|s| format!("reading a resource: {s}"))?;
            if kind != KIND_IOPORT || needs(Need::Ioports) {
                resources.push(duplicate(resource).map_err(|s| format!("a resource: {s}"))?);
            }
        }
        if needs(Need::Dma) {
            let dma = resource(&self.root, KIND_DMA, 0, 0, 0)
                .map_err(|s| format!("creating its DMA resource: {s}"))?;
            resources.push(dma);
        }
        Ok(resources)
    }

    /// The `PRIORITY` startup handle of a driver whose manifest sets
    /// `priority`: none up to 16, and one covering 17 to `priority` up
    /// to 31. Fails above 31, or when there is no resource to make it
    /// from.
    fn priority(&self, priority: i64) -> Result<Option<StartupHandle>, String> {
        if priority > PRIORITY_LAST {
            return Err(format!("its priority {priority} is above 31"));
        }
        if priority < PRIORITY_FIRST {
            return Ok(None);
        }
        let own = self
            .priority
            .as_ref()
            .ok_or_else(|| String::from("there is no PRIORITY resource for its priority"))?;
        let size = (priority - PRIORITY_FIRST + 1) as u64;
        let handle = resource(own, KIND_PRIORITY, PRIORITY_FIRST as u64, size, 0)
            .map_err(|status| format!("creating its PRIORITY resource: {status}"))?;
        Ok(Some(StartupHandle {
            role: HandleRole::Priority,
            id: 0,
            handle,
        }))
    }
}

/// A new resource of `kind` covering `size` from `base`, made from
/// `parent` with `options`.
fn resource(
    parent: &OwnedHandle,
    kind: u32,
    base: u64,
    size: u64,
    options: u32,
) -> Result<OwnedHandle, sys::Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::resource_create(parent.raw(), kind, base, size, options, &mut out) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// The kind of `resource`.
fn kind(resource: &OwnedHandle) -> Result<u32, sys::Status> {
    let mut info = [0u8; INFO_RESOURCE_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `info` and `len`.
    let status = unsafe {
        sys::object_get_info(
            resource.raw(),
            INFO_RESOURCE,
            info.as_mut_ptr(),
            info.len() as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK {
        return Err(status);
    }
    Ok(u32::from_le_bytes([info[0], info[1], info[2], info[3]]))
}

pub(crate) fn duplicate(handle: &OwnedHandle) -> Result<OwnedHandle, sys::Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// An `arguments` memory object that holds only `program`.
fn arguments(program: &str) -> Result<OwnedHandle, sys::Status> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(program.as_bytes());
    bytes.push(0);
    let size = (bytes.len() as u64).next_multiple_of(PAGE_SIZE);
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::mem_create(size, 0, &mut mem) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer and length describe `bytes`.
    let status = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    Ok(mem)
}

/// What a driver is told of `device`: its ID, name, hardware IDs and
/// properties.
pub fn description(device: &Device) -> DeviceDescription {
    DeviceDescription {
        id: device.id,
        name: device.name.clone(),
        hardware_ids: device.hardware_ids.clone(),
        properties: device
            .properties
            .iter()
            .map(|(key, value)| Property {
                key: key.clone(),
                value: value.clone(),
            })
            .collect(),
    }
}
