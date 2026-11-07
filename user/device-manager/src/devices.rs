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
/// whether it failed, which leaves it without a driver for good, and
/// the client of its driver's `Driver` channel while the driver runs.
pub struct Device {
    pub id: u64,
    pub name: String,
    pub hardware_ids: Vec<String>,
    pub properties: Vec<(String, String)>,
    pub parent: Option<u64>,
    pub resources: Vec<OwnedHandle>,
    pub driver: Option<String>,
    pub failed: bool,
    pub client: Option<Rc<DriverAsyncClient>>,
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
            client: None,
        };
        mikros_rt::log!(Info, "device-manager: added {}", describe(&device));
        self.list.push(device);
        id
    }

    pub fn get(&self, id: u64) -> Option<&Device> {
        self.list.iter().find(|d| d.id == id)
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
            device.client = None;
            mikros_rt::log!(
                Error,
                "device-manager: device {id}, {}, failed; it is left without a driver",
                device.name
            );
        }
    }

    /// Removes the devices below device `id`, and logs each one.
    pub fn remove_descendants(&mut self, id: u64) {
        let mut parents = alloc::vec![id];
        while let Some(parent) = parents.pop() {
            while let Some(at) = self.list.iter().position(|d| d.parent == Some(parent)) {
                let child = self.list.remove(at);
                mikros_rt::log!(
                    Info,
                    "device-manager: removed device {}, {}",
                    child.id,
                    child.name
                );
                parents.push(child.id);
            }
        }
    }

    /// Binds a driver of `drivers` to each device without one that has
    /// not failed, and logs which, or that none matches. A device that
    /// several drivers match equally gets the first by name, with a
    /// warning.
    pub fn bind(&mut self, drivers: &[Driver]) {
        let unbound = self
            .list
            .iter_mut()
            .filter(|d| d.driver.is_none() && !d.failed);
        for device in unbound {
            let (id, name) = (device.id, &device.name);
            let Some(choice) = mikros_manifest::choose(drivers, &device.hardware_ids) else {
                mikros_rt::log!(Info, "device-manager: no driver for device {id}, {name}");
                continue;
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
