//! The devices that the ACPI server adds from the namespace. Each has the
//! hardware IDs `acpi:` and its `_HID` and then each `_CID`, in lowercase,
//! the property `path`, its ACPI path, and the resources of its `_CRS`,
//! each requested shared. It is named after the last segment of its path,
//! in lowercase. The PS/2 keyboard and mouse are one device, `i8042`,
//! with the hardware ID `acpi:i8042`, the keyboard's path and the
//! resources of both.

use crate::host::Host;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};
use mikros_acpica::Host as _;
use mikros_acpica::devices::{Device, Kind, Resource};
use mikros_rt::interfaces::mikros::driver::Property;

const FOREVER: i64 = i64::MAX;
const KIND_MMIO: u32 = 1;
const KIND_IOPORT: u32 = 2;
const KIND_IRQ: u32 = 3;
/// The IDs of PS/2 keyboards and mice, as `_HID` or `_CID`.
const KEYBOARDS: [&str; 2] = ["PNP0303", "PNP030B"];
const MICE: [&str; 2] = ["PNP0F03", "PNP0F13"];
const PS2_NAME: &str = "i8042";
const PS2_ID: &str = "acpi:i8042";
/// What `AddDevice` takes at most.
const NAME_MAX: usize = 64;
const ID_MAX: usize = 128;
const IDS_MAX: usize = 16;
const RESOURCES_MAX: usize = 16;

/// Whether `device` has one of `ids` as its `_HID` or a `_CID`.
fn is_one_of(device: &Device, ids: &[&str]) -> bool {
    ids.contains(&device.hid.as_str()) || device.cids.iter().any(|c| ids.contains(&c.as_str()))
}

/// The first `max` bytes of `text`, ending at a character boundary.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// The last segment of the ACPI path `path`, in lowercase.
fn name_of(path: &str) -> String {
    let segment = path.rsplit(['.', '\\', '^']).next().unwrap_or(path);
    cut(&segment.to_lowercase(), NAME_MAX)
}

/// The hardware IDs of `device`.
fn hardware_ids(device: &Device) -> Vec<String> {
    core::iter::once(&device.hid)
        .chain(&device.cids)
        .map(|id| cut(&format!("acpi:{}", id.to_lowercase()), ID_MAX))
        .take(IDS_MAX)
        .collect()
}

/// Adds the devices of the namespace, and gives their IDs.
pub fn add(host: &Host) -> Vec<u64> {
    let devices = match mikros_acpica::devices::devices() {
        Ok(devices) => devices,
        Err(failed) => {
            mikros_rt::log!(Error, "acpi: walking the namespace failed: {failed:#x}");
            return Vec::new();
        }
    };
    let mut added = Vec::new();
    let (mut keyboard, mut mouse) = (None, None);
    for device in devices {
        if keyboard.is_none() && is_one_of(&device, &KEYBOARDS) {
            keyboard = Some(device);
        } else if mouse.is_none() && is_one_of(&device, &MICE) {
            mouse = Some(device);
        } else {
            let ids = hardware_ids(&device);
            added.extend(add_one(
                host,
                name_of(&device.path),
                ids,
                &device.path,
                &device.resources,
            ));
        }
    }
    if let Some(first) = keyboard.as_ref().or(mouse.as_ref()) {
        let resources: Vec<Resource> = keyboard
            .iter()
            .chain(mouse.iter())
            .flat_map(|d| d.resources.iter().copied())
            .collect();
        let ids = vec![PS2_ID.to_string()];
        added.extend(add_one(host, PS2_NAME.into(), ids, &first.path, &resources));
    }
    added
}

/// Adds one device, with a shared resource of the device manager for
/// each of `resources` that it grants: its ID, or None when it could not
/// be added.
fn add_one(
    host: &Host,
    name: String,
    hardware_ids: Vec<String>,
    path: &str,
    resources: &[Resource],
) -> Option<u64> {
    let mut granted = Vec::new();
    for r in resources.iter().take(RESOURCES_MAX) {
        let kind = match r.kind {
            Kind::Io => KIND_IOPORT,
            Kind::Memory => KIND_MMIO,
            Kind::Irq => KIND_IRQ,
        };
        // A refused resource is logged by the host and left out.
        granted.extend(host.request(kind, r.base, r.size, true));
    }
    let properties = vec![Property {
        key: "path".into(),
        value: path.into(),
    }];
    let added = host
        .manager()
        .add_device(name.clone(), hardware_ids, properties, granted, FOREVER);
    match added {
        Ok(Ok(added)) => Some(added.id),
        Ok(Err(status)) => {
            mikros_rt::log!(Error, "acpi: adding {name}, {path}, failed: {status:?}");
            None
        }
        Err(_) => None,
    }
}
