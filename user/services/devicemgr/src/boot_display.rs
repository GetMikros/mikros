//! Which device gets the boot framebuffer: the PCI function that the
//! startup message names, once its bus driver adds it, or else a root
//! device of its own.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

/// The hardware ID added after the own IDs of the PCI function that has
/// the boot framebuffer.
pub const DISPLAY_BOOT_FRAMEBUFFER: &str = "display:boot-framebuffer";
/// The hardware ID of the root device that has the boot framebuffer when
/// no PCI function does.
pub const PLATFORM_BOOT_FRAMEBUFFER: &str = "platform:boot-framebuffer";

/// Who has the boot framebuffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Given {
    /// Nobody yet: its PCI function has not been added.
    Waiting,
    /// The PCI function, also each time it is added again.
    Function,
    /// The root device.
    Platform,
}

/// The boot framebuffer while its PCI function is expected: the
/// function's device name, the framebuffer's properties, the host bridge
/// device whose bus driver adds the function, and who has it.
pub struct BootDisplay {
    pub name: String,
    pub properties: Vec<(String, String)>,
    pub host: u64,
    pub given: Cell<Given>,
}

impl BootDisplay {
    /// Whether the device named `name` that the driver of device `caller`
    /// adds is the framebuffer's PCI function, and gets it.
    pub fn is(&self, caller: u64, name: &str) -> bool {
        self.given.get() != Given::Platform && self.host == caller && self.name == name
    }
}

/// The device name of the PCI function at `pci`, which is its segment
/// times 65536 plus its bus times 256 plus device times 8 plus function.
pub fn function_name(pci: u32) -> String {
    let (segment, bus, devfn) = (pci >> 16, (pci >> 8) & 0xFF, pci & 0xFF);
    format!(
        "pci-{segment:04x}:{bus:02x}:{:02x}.{:x}",
        devfn >> 3,
        devfn & 7
    )
}

/// Whether the host bridge of `segment` with buses from `bus_start` to
/// `bus_end` covers the PCI function at `pci`.
pub fn covers(segment: u16, bus_start: u8, bus_end: u8, pci: u32) -> bool {
    let bus = ((pci >> 8) & 0xFF) as u8;
    u32::from(segment) == pci >> 16 && (bus_start..=bus_end).contains(&bus)
}
