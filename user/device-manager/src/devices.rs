//! The tree of devices.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use mikros_rt::interfaces::__midl::OwnedHandle;

/// A device: its ID, name, hardware IDs from the most specific to the
/// least, properties, parent, unless it is a root device, and the
/// resources of its hardware.
pub struct Device {
    pub id: u64,
    pub name: String,
    pub hardware_ids: Vec<String>,
    pub properties: Vec<(String, String)>,
    pub parent: Option<u64>,
    pub resources: Vec<OwnedHandle>,
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
        };
        mikros_rt::log!(Info, "device-manager: added {}", describe(&device));
        self.list.push(device);
        id
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
