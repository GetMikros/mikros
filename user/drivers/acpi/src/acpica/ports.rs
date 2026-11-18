//! I/O ports for ACPICA and the ACPI server: through `IOPORT` resources
//! that the device manager grants, shared with other drivers, each
//! enabled for the process the first time one of its ports is used. A
//! range that the device manager refused is not asked for again.

use crate::acpica::sync::Locked;
use crate::acpica::{AE_BAD_PARAMETER, AE_ERROR, AE_OK, Refused, Status};
use alloc::vec::Vec;
use core::arch::asm;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const KIND_IOPORT: u32 = 2;

struct Ports {
    /// The first port and the count of each range enabled for the
    /// process, with its resource.
    enabled: Vec<(u16, u16, OwnedHandle)>,
    /// The first port and the count of each range that the device
    /// manager refused, with its status.
    refused: Vec<(u16, u16, Refused)>,
}

static PORTS: Locked<Ports> = Locked::new(Ports {
    enabled: Vec::new(),
    refused: Vec::new(),
});

/// Enables the `count` ports from `port` for the process, unless a range
/// already holds them: the status that refused them otherwise, and
/// whether the device manager refused them in this call.
fn get(port: u16, count: u16) -> Result<(), (Refused, bool)> {
    let end = u32::from(port) + u32::from(count);
    if end > 0x1_0000 {
        return Err((Refused::OutOfRange, false));
    }
    let mut ports = PORTS.lock();
    if ports
        .enabled
        .iter()
        .any(|&(first, n, _)| first <= port && end <= u32::from(first) + u32::from(n))
    {
        return Ok(());
    }
    if let Some(&(_, _, status)) = ports
        .refused
        .iter()
        .find(|&&(first, n, _)| first == port && n == count)
    {
        return Err((status, false));
    }
    let base = u64::from(port);
    let resource = match crate::acpica::host().request(KIND_IOPORT, base, u64::from(count), true) {
        Ok(resource) => resource,
        Err(status) => {
            ports.refused.push((port, count, status));
            return Err((status, true));
        }
    };
    // SAFETY: the call takes no pointers.
    let status = unsafe {
        sys::process_ioports_enable(
            crate::acpica::process(),
            resource.raw(),
            base,
            u64::from(count),
        )
    };
    if status != mikros_rt::OK {
        return Err((Refused::from_raw(status as i32), false));
    }
    ports.enabled.push((port, count, resource));
    Ok(())
}

/// Enables the `count` ports from `port` for the process, unless a range
/// already holds them: false when the device manager or the kernel
/// refused, which is logged when the device manager refused them first.
pub(crate) fn enable(port: u16, count: u16) -> bool {
    match get(port, count) {
        Ok(()) => true,
        Err((status, refused)) => {
            if refused {
                mikros_rt::log!(
                    Warning,
                    "acpi: resource {KIND_IOPORT} at {port:#x}, {count:#x} long: {status:?}"
                );
            }
            false
        }
    }
}

/// The `width` bits, 8, 16 or 32, at port `port`: None when the port
/// cannot be used.
pub fn read(port: u16, width: u32) -> Option<u32> {
    if !matches!(width, 8 | 16 | 32) || !enable(port, (width / 8) as u16) {
        return None;
    }
    // SAFETY: the process may use the ports, which only the hardware
    // reads.
    unsafe {
        Some(match width {
            8 => {
                let value: u8;
                asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
                u32::from(value)
            }
            16 => {
                let value: u16;
                asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
                u32::from(value)
            }
            _ => {
                let value: u32;
                asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags));
                value
            }
        })
    }
}

/// Writes the low `width` bits, 8, 16 or 32, of `value` to port `port`:
/// false when the port cannot be used.
pub fn write(port: u16, value: u32, width: u32) -> bool {
    if !matches!(width, 8 | 16 | 32) || !enable(port, (width / 8) as u16) {
        return false;
    }
    // SAFETY: the process may use the ports, which only the hardware
    // reads.
    unsafe {
        match width {
            8 => {
                asm!("out dx, al", in("dx") port, in("al") value as u8, options(nomem, nostack, preserves_flags))
            }
            16 => {
                asm!("out dx, ax", in("dx") port, in("ax") value as u16, options(nomem, nostack, preserves_flags))
            }
            _ => {
                asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags))
            }
        }
    }
    true
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsReadPort(port: u64, value: *mut u32, width: u32) -> Status {
    let Ok(port) = u16::try_from(port) else {
        return AE_BAD_PARAMETER;
    };
    if value.is_null() {
        return AE_BAD_PARAMETER;
    }
    match read(port, width) {
        Some(read) => {
            // SAFETY: ACPICA passes a pointer to write the value to.
            unsafe { *value = read };
            AE_OK
        }
        None => AE_ERROR,
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsWritePort(port: u64, value: u32, width: u32) -> Status {
    let Ok(port) = u16::try_from(port) else {
        return AE_BAD_PARAMETER;
    };
    if write(port, value, width) {
        AE_OK
    } else {
        AE_ERROR
    }
}
