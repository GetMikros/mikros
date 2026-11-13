//! The routing of PCI interrupt pins, through the glue: the GSI, trigger
//! mode and polarity of a device's pin.

use crate::acpica::{AE_OK, Status};

const LEVEL: u32 = 1 << 0;
const ACTIVE_LOW: u32 = 1 << 1;

unsafe extern "C" {
    fn mikros_acpi_use_apic() -> Status;
    fn mikros_acpi_pci_route(
        segment: u16,
        bus: u8,
        device: u8,
        pin: u8,
        gsi: *mut u32,
        flags: *mut u32,
    ) -> Status;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub gsi: u32,
    pub level: bool,
    pub active_low: bool,
}

/// Tells the firmware that interrupts are routed through I/O APICs, when
/// it has `\_PIC`.
pub(crate) fn use_apic() -> Result<(), Status> {
    // SAFETY: the glue takes no arguments.
    match unsafe { mikros_acpi_use_apic() } {
        AE_OK => Ok(()),
        status => Err(status),
    }
}

/// The route of interrupt pin `pin`, 1 for INTA to 4 for INTD, of device
/// `device` on bus `bus` of segment `segment`: `AE_NOT_FOUND` when no
/// routing entry applies.
pub fn pci_route(segment: u16, bus: u8, device: u8, pin: u8) -> Result<Route, Status> {
    let (mut gsi, mut flags) = (0, 0);
    // SAFETY: the pointers are valid for the call.
    let status = unsafe { mikros_acpi_pci_route(segment, bus, device, pin, &mut gsi, &mut flags) };
    match status {
        AE_OK => Ok(Route {
            gsi,
            level: flags & LEVEL != 0,
            active_low: flags & ACTIVE_LOW != 0,
        }),
        status => Err(status),
    }
}
