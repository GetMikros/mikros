//! I/O ports for ACPICA and the ACPI server: through `IOPORT` resources
//! that the device manager grants, shared with other drivers, each
//! enabled for the process the first time one of its ports is used.

use crate::sync::Locked;
use crate::{AE_BAD_PARAMETER, AE_ERROR, AE_OK, Status};
use alloc::vec::Vec;
use core::arch::asm;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const KIND_IOPORT: u32 = 2;

/// The first port and the count of each range enabled for the process,
/// with its resource.
static ENABLED: Locked<Vec<(u16, u16, OwnedHandle)>> = Locked::new(Vec::new());

/// Enables the `count` ports from `port` for the process, unless a range
/// already holds them: false when the device manager or the kernel
/// refused.
pub(crate) fn enable(port: u16, count: u16) -> bool {
    let end = u32::from(port) + u32::from(count);
    if end > 0x1_0000 {
        return false;
    }
    let mut enabled = ENABLED.lock();
    if enabled
        .iter()
        .any(|&(first, n, _)| first <= port && end <= u32::from(first) + u32::from(n))
    {
        return true;
    }
    let base = u64::from(port);
    let Some(resource) = crate::host().request(KIND_IOPORT, base, u64::from(count), true) else {
        return false;
    };
    // SAFETY: the call takes no pointers.
    let status = unsafe {
        sys::process_ioports_enable(crate::process(), resource.raw(), base, u64::from(count))
    };
    if status != mikros_rt::OK {
        return false;
    }
    enabled.push((port, count, resource));
    true
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
