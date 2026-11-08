//! The tree of devices, and the drivers bound to them.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use mikros_manifest::Driver;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::driver::DriverAsyncClient;

/// A device: its ID, name, hardware IDs from the most specific to the
/// least, properties, parent, unless it is a root device, the
/// resources of its hardware, the name of its driver once one is bound,
/// whether it failed, which leaves it without a driver for good, the
/// process ID of its driver and the client of the driver's `Driver`
/// channel once the driver was spawned, whether it is being removed,
/// which stops its driver for good, how many times its driver was
/// started, and, while it waits to be added again by its parent's
/// restarted driver, the start of that driver it waits for.
pub struct Device {
    pub id: u64,
    pub name: String,
    pub hardware_ids: Vec<String>,
    pub properties: Vec<(String, String)>,
    pub parent: Option<u64>,
    pub resources: Vec<OwnedHandle>,
    pub driver: Option<String>,
    pub failed: bool,
    pub pid: Option<u32>,
    pub client: Option<Rc<DriverAsyncClient>>,
    pub removing: bool,
    pub starts: u64,
    pub kept: Option<u64>,
}

/// The devices, with the ID the next one gets.
pub struct Devices {
    list: Vec<Device>,
    next_id: u64,
}

impl Devices {
    pub fn new() -> Devices {
        Devices {
            list: Vec::new(),
            next_id: 1,
        }
    }

    /// Adds a device with a new ID, which it returns, and logs it with
    /// its first hardware ID, parent, resource count and properties.
    pub fn add(
        &mut self,
        name: String,
        hardware_ids: Vec<String>,
        properties: Vec<(String, String)>,
        parent: Option<u64>,
        resources: Vec<OwnedHandle>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let device = Device {
            id,
            name,
            hardware_ids,
            properties,
            parent,
            resources,
            driver: None,
            failed: false,
            pid: None,
            client: None,
            removing: false,
            starts: 0,
            kept: None,
        };
        mikros_rt::log!(Info, "device-manager: added {}", describe(&device));
        self.list.push(device);
        id
    }

    pub fn get(&self, id: u64) -> Option<&Device> {
        self.list.iter().find(|d| d.id == id)
    }

    /// The devices, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &Device> {
        self.list.iter()
    }

    /// Keeps the children of device `id` for its driver's next start,
    /// `start`: until that driver adds them again or they are removed.
    pub fn keep_children(&mut self, id: u64, start: u64) {
        for child in self.list.iter_mut().filter(|d| d.parent == Some(id)) {
            child.kept = Some(start);
        }
    }

    /// The children of device `id` that are still kept for its driver's
    /// start `start`.
    pub fn kept_children(&self, id: u64, start: u64) -> Vec<u64> {
        let kept = self
            .list
            .iter()
            .filter(|d| d.parent == Some(id) && d.kept == Some(start));
        kept.map(|d| d.id).collect()
    }

    /// The kept child of device `parent` named `name` with the hardware
    /// IDs `ids`, which is no longer kept from then on, and logged as
    /// added again.
    pub fn take_kept(&mut self, parent: u64, name: &str, ids: &[String]) -> Option<u64> {
        let child = self.list.iter_mut().find(|d| {
            d.parent == Some(parent) && d.kept.is_some() && d.name == name && d.hardware_ids == ids
        })?;
        child.kept = None;
        mikros_rt::log!(
            Info,
            "device-manager: added device {} again, {}, under {parent}",
            child.id,
            child.name
        );
        Some(child.id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Device> {
        self.list.iter_mut().find(|d| d.id == id)
    }

    /// The IDs of the devices that have a driver bound, in the order
    /// they were added.
    pub fn bound(&self) -> Vec<u64> {
        let bound = self.list.iter().filter(|d| d.driver.is_some());
        bound.map(|d| d.id).collect()
    }

    /// Marks device `id` as failed and leaves it without a driver, and
    /// logs it.
    pub fn fail(&mut self, id: u64) {
        if let Some(device) = self.get_mut(id) {
            device.failed = true;
            device.driver = None;
            device.pid = None;
            device.client = None;
            mikros_rt::log!(
                Error,
                "device-manager: device {id}, {}, failed; it is left without a driver",
                device.name
            );
        }
    }

    /// The IDs of the children of device `id`, in the order they were
    /// added.
    pub fn children(&self, id: u64) -> Vec<u64> {
        let children = self.list.iter().filter(|d| d.parent == Some(id));
        children.map(|d| d.id).collect()
    }

    /// Removes device `id` from the list, and logs it.
    pub fn remove(&mut self, id: u64) {
        if let Some(at) = self.list.iter().position(|d| d.id == id) {
            let device = self.list.remove(at);
            mikros_rt::log!(Info, "device-manager: removed device {id}, {}", device.name);
        }
    }

    /// Binds a driver of `drivers` to each device without one that has
    /// not failed, as `bind_device` does.
    pub fn bind(&mut self, drivers: &[Driver]) {
        let unbound = self.list.iter().filter(|d| d.driver.is_none() && !d.failed);
        let ids: Vec<u64> = unbound.map(|d| d.id).collect();
        for id in ids {
            self.bind_device(id, drivers);
        }
    }

    /// Binds a driver of `drivers` to device `id`, and logs which, or
    /// that none matches. When several drivers match it equally, it gets
    /// the first by name, with a warning. Returns whether one was bound.
    pub fn bind_device(&mut self, id: u64, drivers: &[Driver]) -> bool {
        let Some(device) = self.get_mut(id) else {
            return false;
        };
        let name = &device.name;
        {
            let Some(choice) = mikros_manifest::choose(drivers, &device.hardware_ids) else {
                mikros_rt::log!(Info, "device-manager: no driver for device {id}, {name}");
                return false;
            };
            let (driver, matched) = (&choice.driver.name, choice.hardware_id);
            if !choice.passed_over.is_empty() {
                mikros_rt::log!(
                    Warning,
                    "device-manager: drivers {:?} also match device {id} as {matched}",
                    choice.passed_over
                );
            }
            mikros_rt::log!(
                Info,
                "device-manager: bound driver {driver} to device {id}, {name}, as {matched}"
            );
            device.driver = Some(driver.clone());
        }
        true
    }
}

/// `device` in words: its ID, name, first hardware ID, the parent of a
/// device that has one, its resource count and its properties.
fn describe(device: &Device) -> String {
    let first = device.hardware_ids.first().map_or("", String::as_str);
    let mut text = alloc::format!("device {}, {}, as {first}", device.id, device.name);
    if let Some(parent) = device.parent {
        let _ = write!(text, ", under {parent}");
    }
    let count = device.resources.len();
    let plural = if count == 1 { "" } else { "s" };
    let _ = write!(text, ", with {count} resource{plural}");
    for (key, value) in &device.properties {
        let _ = write!(text, ", {key} {value}");
    }
    text
}
