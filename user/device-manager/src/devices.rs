//! The tree of devices, and the drivers bound to them.

use crate::start::{Running, Starter};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use mikros_manifest::Driver;
use mikros_rt::interfaces::__midl::OwnedHandle;

/// A device: its ID, name, hardware IDs from the most specific to the
/// least, properties, parent, unless it is a root device, the
/// resources of its hardware, the name of its driver once one is bound,
/// and that driver once it was spawned.
pub struct Device {
    pub id: u64,
    pub name: String,
    pub hardware_ids: Vec<String>,
    pub properties: Vec<(String, String)>,
    pub parent: Option<u64>,
    pub resources: Vec<OwnedHandle>,
    pub driver: Option<String>,
    pub running: Option<Running>,
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
            running: None,
        };
        mikros_rt::log!(Info, "device-manager: added {}", describe(&device));
        self.list.push(device);
        id
    }

    /// Binds a driver of `drivers` to each device without one, and
    /// logs which, or that none matches. A device that several drivers
    /// match equally gets the first by name, with a warning.
    pub fn bind(&mut self, drivers: &[Driver]) {
        for device in self.list.iter_mut().filter(|d| d.driver.is_none()) {
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

    /// Starts the driver of `drivers` bound to each device whose driver
    /// was not spawned, in the order the devices were added, and logs
    /// each one that does not start.
    pub async fn start(&mut self, drivers: &[Driver], starter: &Starter) {
        for device in self.list.iter_mut().filter(|d| d.running.is_none()) {
            let Some(bound) = &device.driver else {
                continue;
            };
            let Some(driver) = drivers.iter().find(|d| &d.name == bound) else {
                continue;
            };
            match starter.start(device, driver).await {
                Ok(running) => device.running = Some(running),
                Err(why) => mikros_rt::log!(
                    Error,
                    "device-manager: driver {bound} for device {} did not start: {why}",
                    device.id
                ),
            }
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
