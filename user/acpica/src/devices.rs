//! The devices of the ACPI namespace that the ACPI server adds, from the
//! glue's walk of the namespace, with their hardware IDs as ACPI writes
//! them, their paths, and the resources of their `_CRS`. The walk goes on
//! below PCI host bridges and the objects of PCI functions, so that the
//! devices of an LPC bridge are found.

use crate::{AE_OK, Status};
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_void};

/// The glue's resource kinds, and the flags of its interrupts.
const KIND_IO: u32 = 1;
const KIND_MEMORY: u32 = 2;
const KIND_IRQ: u32 = 3;
const LEVEL: u32 = 1 << 0;
const ACTIVE_LOW: u32 = 1 << 1;
const SHARED: u32 = 1 << 2;
const CIDS_MAX: usize = 8;
const RESOURCES_MAX: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Io,
    Memory,
    Irq,
}

/// A resource of a device: `size` ports or bytes from `base`, or the
/// interrupt `base` with its trigger mode, polarity and sharing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resource {
    pub kind: Kind,
    pub base: u64,
    pub size: u64,
    pub level: bool,
    pub active_low: bool,
    pub shared: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The device's full path in the namespace.
    pub path: String,
    pub hid: String,
    pub cids: Vec<String>,
    pub resources: Vec<Resource>,
}

/// The glue's `struct mikros_acpi_resource`.
#[repr(C)]
struct RawResource {
    kind: u32,
    flags: u32,
    base: u64,
    size: u64,
}

/// The glue's `struct mikros_acpi_device`.
#[repr(C)]
struct RawDevice {
    path: *const c_char,
    hid: *const c_char,
    cids: [*const c_char; CIDS_MAX],
    cid_count: u32,
    resources: [RawResource; RESOURCES_MAX],
    resource_count: u32,
}

type Found = extern "C" fn(*mut c_void, *const RawDevice);

unsafe extern "C" {
    fn mikros_acpi_devices(found: Found, context: *mut c_void) -> Status;
    fn mikros_acpi_century() -> u8;
}

/// The CMOS index of the real-time clock's century register that the
/// FADT names: None when it names none.
pub fn century() -> Option<u8> {
    // SAFETY: the glue takes no arguments; ACPICA has parsed the FADT
    // once it runs.
    match unsafe { mikros_acpi_century() } {
        0 => None,
        index => Some(index),
    }
}

/// The string at `text`, or an empty one for none.
///
/// # Safety
///
/// `text` must be null or point to a string that ends with a 0.
unsafe fn string(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: the caller passes a string that ends with a 0.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

extern "C" fn found(context: *mut c_void, raw: *const RawDevice) {
    // SAFETY: `devices` passes its vector; the glue passes a device, and
    // strings, that last for the call.
    let (devices, raw) = unsafe { (&mut *(context as *mut Vec<Device>), &*raw) };
    let cids = raw.cids[..(raw.cid_count as usize).min(CIDS_MAX)]
        .iter()
        // SAFETY: as above.
        .map(|&cid| unsafe { string(cid) })
        .collect();
    let resources = raw.resources[..(raw.resource_count as usize).min(RESOURCES_MAX)]
        .iter()
        .filter_map(|r| {
            let kind = match r.kind {
                KIND_IO => Kind::Io,
                KIND_MEMORY => Kind::Memory,
                KIND_IRQ => Kind::Irq,
                _ => return None,
            };
            Some(Resource {
                kind,
                base: r.base,
                size: r.size,
                level: r.flags & LEVEL != 0,
                active_low: r.flags & ACTIVE_LOW != 0,
                shared: r.flags & SHARED != 0,
            })
        })
        .collect();
    devices.push(Device {
        // SAFETY: as above.
        path: unsafe { string(raw.path) },
        // SAFETY: as above.
        hid: unsafe { string(raw.hid) },
        cids,
        resources,
    });
}

/// The devices of the namespace whose `_STA` says that they are present
/// and enabled and that have a `_HID`, other than PCI host bridges, in
/// the order of the namespace, with their paths without the underscores
/// that pad names, and legacy IRQs as GSIs.
pub fn devices() -> Result<Vec<Device>, Status> {
    let mut devices = Vec::new();
    // SAFETY: `found` takes the vector as its context, for the call.
    let status =
        unsafe { mikros_acpi_devices(found, &mut devices as *mut Vec<Device> as *mut c_void) };
    match status {
        AE_OK => Ok(devices),
        status => Err(status),
    }
}
