//! The hardware the kernel keeps for itself and the memory that is not
//! device memory, which resources may not grant.

use crate::memory;
use alloc::vec::Vec;
use boot_protocol::{BootInfo, Kind, PAGE_SIZE};
use kernel_core::object::resource::{KIND_IOPORT, KIND_MMIO};
use kernel_core::sync::SpinLock;

/// The I/O ports of the PICs, the PIT and the PIT gate.
const PORTS: [(u64, u64); 4] = [(0x20, 0x22), (0xA0, 0xA2), (0x40, 0x44), (0x61, 0x62)];
/// The ports of the debug serial port.
const SERIAL_PORTS: (u64, u64) = (0x3F8, 0x400);

/// The memory map kinds that are never device memory.
const NOT_DEVICE: [Kind; 8] = [
    Kind::Usable,
    Kind::BootloaderReclaimable,
    Kind::KernelImage,
    Kind::KernelStack,
    Kind::BootImage,
    Kind::PageTables,
    Kind::ApTrampoline,
    Kind::Bad,
];

/// Ranges by resource kind, start and exclusive end.
static RANGES: SpinLock<Vec<(u32, u64, u64)>> = SpinLock::new(Vec::new());

/// Keeps the range from `start` to `end`, exclusive, of resource kind
/// `kind` from being granted.
pub fn reserve(kind: u32, start: u64, end: u64) {
    RANGES.lock().push((kind, start, end));
}

/// Keeps the pages holding the `size` bytes at `phys` from being granted.
pub fn reserve_mmio(phys: u64, size: u64) {
    let start = phys / PAGE_SIZE * PAGE_SIZE;
    reserve(KIND_MMIO, start, (phys + size).next_multiple_of(PAGE_SIZE));
}

/// Reserves the fixed I/O ports, the serial port when it is present and
/// the memory that is not device memory. Must run while the memory map
/// is still in place.
pub fn init(info: &BootInfo) {
    for (start, end) in PORTS {
        reserve(KIND_IOPORT, start, end);
    }
    if crate::serial::present() {
        reserve(KIND_IOPORT, SERIAL_PORTS.0, SERIAL_PORTS.1);
    }
    let not_device =
        memory::memory_map(info).filter(|e| NOT_DEVICE.iter().any(|&k| k as u32 == e.kind));
    for e in not_device {
        reserve(
            KIND_MMIO,
            e.phys_start,
            e.phys_start + e.page_count * PAGE_SIZE,
        );
    }
}

/// Whether a range of `kind` from `start` to `end`, exclusive, overlaps a
/// reserved one.
pub fn denied(kind: u32, start: u64, end: u64) -> bool {
    RANGES
        .lock()
        .iter()
        .any(|&(k, s, e)| k == kind && s < end && start < e)
}
