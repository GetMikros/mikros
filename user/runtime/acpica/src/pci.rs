//! PCI configuration space for ACPICA: through the ECAM ranges of the
//! MCFG, which `ranges` reads once the tables are found, and which are
//! mapped a bus at a time, the first time a function of the bus is used.

use crate::sync::Locked;
use crate::{AE_BAD_PARAMETER, AE_NOT_EXIST, AE_OK, Status, physical};
use alloc::vec::Vec;

/// The configuration space of one bus, and of one function.
const BUS_SIZE: u64 = 1 << 20;
const FUNCTION_SIZE: u32 = 4096;

/// An ECAM range: its base, segment, and first and last bus.
#[derive(Clone, Copy)]
struct Range {
    base: u64,
    segment: u16,
    first: u8,
    last: u8,
}

static RANGES: Locked<Vec<Range>> = Locked::new(Vec::new());
/// The segment and number of each bus mapped, and where.
static BUSES: Locked<Vec<(u16, u8, u64)>> = Locked::new(Vec::new());

unsafe extern "C" {
    fn mikros_acpi_ecam(
        index: u32,
        base: *mut u64,
        segment: *mut u16,
        first: *mut u8,
        last: *mut u8,
    ) -> Status;
}

/// Reads the ECAM ranges of the MCFG; none without one.
pub fn ranges() {
    let mut ranges = RANGES.lock();
    ranges.clear();
    for index in 0.. {
        let mut range = Range {
            base: 0,
            segment: 0,
            first: 0,
            last: 0,
        };
        // SAFETY: the pointers are valid for the call.
        let status = unsafe {
            mikros_acpi_ecam(
                index,
                &mut range.base,
                &mut range.segment,
                &mut range.first,
                &mut range.last,
            )
        };
        if status != AE_OK {
            break;
        }
        ranges.push(range);
    }
}

/// Where the configuration space of `bus` of `segment` is mapped,
/// mapping it the first time: None when no ECAM range holds it or it
/// cannot be mapped.
fn bus(segment: u16, bus: u8) -> Option<u64> {
    let mut buses = BUSES.lock();
    if let Some(&(_, _, at)) = buses.iter().find(|&&(s, b, _)| s == segment && b == bus) {
        return Some(at);
    }
    let range = RANGES
        .lock()
        .iter()
        .copied()
        .find(|r| r.segment == segment && (r.first..=r.last).contains(&bus))?;
    let base = range.base + u64::from(bus - range.first) * BUS_SIZE;
    // The mapping stays for as long as the process runs.
    let at = physical::map(base, BUS_SIZE)?;
    buses.push((segment, bus, at));
    Some(at)
}

/// What ACPICA names a PCI function by.
#[repr(C)]
struct PciId {
    segment: u16,
    bus: u16,
    device: u16,
    function: u16,
}

/// The address of `register` of the function `id` for an access of
/// `width` bits, which must lie in its space and be aligned to them.
fn address(id: *const PciId, register: u32, width: u32) -> Result<u64, Status> {
    if id.is_null() || !matches!(width, 8 | 16 | 32 | 64) {
        return Err(AE_BAD_PARAMETER);
    }
    // SAFETY: ACPICA passes a valid ID.
    let id = unsafe { &*id };
    let bytes = width / 8;
    let (Ok(number), true, true) = (
        u8::try_from(id.bus),
        id.device < 32 && id.function < 8,
        register.is_multiple_of(bytes) && register + bytes <= FUNCTION_SIZE,
    ) else {
        return Err(AE_BAD_PARAMETER);
    };
    let at = bus(id.segment, number).ok_or(AE_NOT_EXIST)?;
    let function = (u64::from(id.device) << 15) | (u64::from(id.function) << 12);
    Ok(at + function + u64::from(register))
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsReadPciConfiguration(
    id: *const PciId,
    register: u32,
    value: *mut u64,
    width: u32,
) -> Status {
    if value.is_null() {
        return AE_BAD_PARAMETER;
    }
    match address(id, register, width) {
        Ok(at) => {
            // SAFETY: the bus is mapped, and `address` checked the access;
            // ACPICA passes a pointer to write the value to.
            unsafe { *value = physical::read(at, width).unwrap_or(0) };
            AE_OK
        }
        Err(status) => status,
    }
}

#[unsafe(no_mangle)]
extern "C" fn AcpiOsWritePciConfiguration(
    id: *const PciId,
    register: u32,
    value: u64,
    width: u32,
) -> Status {
    match address(id, register, width) {
        Ok(at) => {
            // SAFETY: the bus is mapped, and `address` checked the access.
            unsafe { physical::write(at, value, width) };
            AE_OK
        }
        Err(status) => status,
    }
}
