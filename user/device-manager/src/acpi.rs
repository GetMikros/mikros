//! Reading the ACPI tables that the device manager needs from physical
//! memory: the RSDP that the platform information names, the XSDT or
//! RSDT it points to, and the MCFG table.

use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

const PAGE_SIZE: u64 = 4096;
const CACHED: u32 = 0;
const PERM_READ: u32 = 1 << 0;
/// The size of a system description table's header.
const HEADER_SIZE: usize = 36;

/// The physical memory that tables are read from: the `ROOT` resource
/// and the process to map it into.
pub struct Memory<'a> {
    pub root: &'a OwnedHandle,
    pub process: sys::Handle,
}

impl Memory<'_> {
    /// The `len` bytes at physical address `phys`, or `None` when they
    /// cannot be mapped.
    fn read(&self, phys: u64, len: usize) -> Option<Vec<u8>> {
        let start = phys & !(PAGE_SIZE - 1);
        let end = phys.checked_add(len as u64)?.next_multiple_of(PAGE_SIZE);
        let mut mem = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::mem_create_physical(self.root.raw(), start, end - start, CACHED, &mut mem)
        };
        if status != mikros_rt::OK {
            return None;
        }
        // SAFETY: the kernel just created the handle for us.
        let mem = unsafe { OwnedHandle::from_raw(mem) };
        let mut at = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::vm_map(
                self.process,
                PERM_READ,
                0,
                mem.raw(),
                0,
                end - start,
                &mut at,
            )
        };
        if status != mikros_rt::OK {
            return None;
        }
        let offset = (phys - start) as usize;
        // SAFETY: the range is mapped readable at `at` until it is unmapped
        // below.
        let bytes =
            unsafe { core::slice::from_raw_parts((at as usize + offset) as *const u8, len) };
        let copy = bytes.to_vec();
        // SAFETY: the call takes no pointers.
        unsafe { sys::vm_unmap(self.process, at, end - start) };
        Some(copy)
    }

    /// The table at `phys` with its whole length, if its checksum holds.
    fn table(&self, phys: u64) -> Option<Vec<u8>> {
        let header = self.read(phys, HEADER_SIZE)?;
        let len = u32_at(&header, 4)? as usize;
        if len < HEADER_SIZE {
            return None;
        }
        let table = self.read(phys, len)?;
        sums_to_zero(&table).then_some(table)
    }

    /// The table with `signature` that the RSDP at `rsdp` lists, through
    /// the XSDT when the RSDP has one and the RSDT otherwise.
    pub fn find(&self, rsdp: u64, signature: &[u8; 4]) -> Option<Vec<u8>> {
        let pointer = self.read(rsdp, 36)?;
        if &pointer[..8] != b"RSD PTR " || !sums_to_zero(&pointer[..20]) {
            return None;
        }
        let xsdt = (pointer[15] >= 2).then(|| u64_at(&pointer, 24)).flatten();
        let (root, entry) = match xsdt.filter(|&x| x != 0) {
            Some(xsdt) => (self.table(xsdt)?, 8),
            None => (self.table(u64::from(u32_at(&pointer, 16)?))?, 4),
        };
        root[HEADER_SIZE..]
            .chunks_exact(entry)
            .map(|e| match entry {
                8 => u64_at(e, 0),
                _ => u32_at(e, 0).map(u64::from),
            })
            .filter_map(|phys| self.table(phys?))
            .find(|table| &table[..4] == signature)
    }
}

/// A PCI segment's configuration space, from an MCFG entry.
pub struct Ecam {
    pub base: u64,
    pub segment: u16,
    pub bus_start: u8,
    pub bus_end: u8,
}

/// The entries of the MCFG table `mcfg`, which follow its header and 8
/// reserved bytes.
pub fn ecams(mcfg: &[u8]) -> impl Iterator<Item = Ecam> + '_ {
    let (entries, _) = mcfg.get(HEADER_SIZE + 8..).unwrap_or(&[]).as_chunks::<16>();
    entries
        .iter()
        .map(|e| Ecam {
            base: u64::from_le_bytes(e[..8].try_into().expect("8 bytes")),
            segment: u16::from_le_bytes([e[8], e[9]]),
            bus_start: e[10],
            bus_end: e[11],
        })
        .filter(|e| e.bus_start <= e.bus_end)
}

fn sums_to_zero(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b)) == 0
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
