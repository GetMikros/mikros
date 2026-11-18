//! The resources of the accesses of operation regions, which the glue's
//! handlers of SystemMemory and SystemIO regions, in `glue/regions.c`,
//! ask for before each access, and the log of a region whose resource
//! was refused (MIKSPEC-0038.R24). The glue logs such a region once, and
//! fails each access whose resource was refused with AE_ACCESS. ACPICA
//! reports every failure of a handler, but the server does not log its
//! report of AE_ACCESS, which only these handlers return
//! ([`handler_refused`]).

use crate::acpica::{Refused, physical, ports};
use core::ffi::{CStr, c_char};

/// ACPICA's address spaces.
const SPACE_MEMORY: u32 = 0;
const SPACE_IO: u32 = 1;

/// What the glue takes of a result: 0 when the resource is held, the
/// status that refused it otherwise.
fn code(held: Result<(), Refused>) -> i32 {
    match held {
        Ok(()) => 0,
        Err(status) => status.into_raw(),
    }
}

/// Maps the `len` bytes at `address`, and gives in `at` where the first
/// is, until `AcpiOsUnmapMemory`.
#[unsafe(no_mangle)]
extern "C" fn mikros_acpi_hold_memory(address: u64, len: u64, at: *mut u64) -> i32 {
    code(physical::hold(address, len).map(|mapped| {
        // SAFETY: the glue passes a pointer to write the address to.
        unsafe { *at = mapped };
    }))
}

/// Enables the `count` ports from `port`.
#[unsafe(no_mangle)]
extern "C" fn mikros_acpi_hold_ports(port: u64, count: u64) -> i32 {
    code(
        u16::try_from(port)
            .ok()
            .zip(u16::try_from(count).ok())
            .ok_or(Refused::OutOfRange)
            .and_then(|(port, count)| ports::hold(port, count)),
    )
}

#[unsafe(no_mangle)]
extern "C" fn mikros_acpi_region_refused(
    path: *const c_char,
    space: u32,
    address: u64,
    len: u64,
    status: i32,
) {
    let path = if path.is_null() {
        "?".into()
    } else {
        // SAFETY: the glue passes a string that ends with a 0.
        unsafe { CStr::from_ptr(path) }.to_string_lossy()
    };
    let space = match space {
        SPACE_MEMORY => "SystemMemory",
        SPACE_IO => "SystemIO",
        _ => "another space",
    };
    mikros_rt::log!(
        Warning,
        "acpi: operation region {path}, {space} at {address:#x}, {len:#x} long: {:?}",
        Refused::from_raw(status)
    );
}

/// Whether `line` of ACPICA's output reports that a region's handler
/// failed with AE_ACCESS, which only the glue's handlers return, for an
/// access whose resource was refused.
pub(crate) fn handler_refused(line: &str) -> bool {
    line.starts_with("ACPI Error: AE_ACCESS, Returned by Handler for [")
}
