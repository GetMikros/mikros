//! Building the kernel's memory map from the firmware's, without allocating.

use boot_protocol::{Kind, MemoryMapEntry, PAGE_SIZE};
use core::fmt;

/// UEFI memory types.
mod uefi_type {
    pub const LOADER_CODE: u32 = 1;
    pub const LOADER_DATA: u32 = 2;
    pub const BOOT_SERVICES_CODE: u32 = 3;
    pub const BOOT_SERVICES_DATA: u32 = 4;
    pub const RUNTIME_SERVICES_CODE: u32 = 5;
    pub const RUNTIME_SERVICES_DATA: u32 = 6;
    pub const CONVENTIONAL: u32 = 7;
    pub const UNUSABLE: u32 = 8;
    pub const ACPI_RECLAIM: u32 = 9;
    pub const ACPI_NVS: u32 = 10;
    pub const MMIO: u32 = 11;
    pub const MMIO_PORT_SPACE: u32 = 12;
}

/// The UEFI memory attribute of ranges that the runtime services use.
pub const EFI_MEMORY_RUNTIME: u64 = 1 << 63;

/// The kind of a range of the given UEFI memory type and attributes. A
/// range with `EFI_MEMORY_RUNTIME` always gets a firmware runtime kind:
/// `FirmwareRuntime` when its type would give another kind.
pub fn firmware_kind(uefi_type: u32, attribute: u64) -> Kind {
    use uefi_type::*;
    let runtime = attribute & EFI_MEMORY_RUNTIME != 0;
    let kind = match uefi_type {
        CONVENTIONAL | BOOT_SERVICES_CODE | BOOT_SERVICES_DATA => Kind::Usable,
        LOADER_CODE | LOADER_DATA => Kind::BootloaderReclaimable,
        RUNTIME_SERVICES_CODE => Kind::FirmwareRuntimeCode,
        RUNTIME_SERVICES_DATA => Kind::FirmwareRuntime,
        ACPI_RECLAIM => Kind::AcpiReclaimable,
        ACPI_NVS => Kind::AcpiNvs,
        MMIO | MMIO_PORT_SPACE if runtime => Kind::FirmwareRuntimeMmio,
        MMIO | MMIO_PORT_SPACE => Kind::Mmio,
        UNUSABLE => Kind::Bad,
        _ => Kind::Reserved,
    };
    if runtime && !kind.is_firmware_runtime() {
        Kind::FirmwareRuntime
    } else {
        kind
    }
}

/// A physical range of whole pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub pages: u64,
    pub kind: Kind,
}

impl Region {
    /// The pages that cover `size` bytes from `start`.
    pub fn covering(start: u64, size: u64, kind: Kind) -> Region {
        let first = start / PAGE_SIZE * PAGE_SIZE;
        let end = (start + size).next_multiple_of(PAGE_SIZE);
        Region {
            start: first,
            pages: (end - first) / PAGE_SIZE,
            kind,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.pages * PAGE_SIZE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The buffer has no room for another entry.
    Full,
    /// Two firmware ranges overlap.
    Overlap(u64),
    /// A range the direct map covers ends above the limit.
    AboveDirectMapLimit(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Full => write!(f, "the memory map buffer is full"),
            Error::Overlap(at) => write!(f, "firmware memory ranges overlap at {at:#x}"),
            Error::AboveDirectMapLimit(end) => {
                write!(
                    f,
                    "memory that must be mapped ends at {end:#x}, above 64 TiB"
                )
            }
        }
    }
}

fn entry(r: Region) -> MemoryMapEntry {
    MemoryMapEntry {
        phys_start: r.start,
        page_count: r.pages,
        kind: r.kind as u32,
        reserved: 0,
    }
}

fn region(e: &MemoryMapEntry) -> Region {
    Region {
        start: e.phys_start,
        pages: e.page_count,
        kind: Kind::from_u32(e.kind).unwrap_or(Kind::Reserved),
    }
}

/// A memory map built in a caller-provided buffer.
pub struct Map<'a> {
    buf: &'a mut [MemoryMapEntry],
    len: usize,
}

impl<'a> Map<'a> {
    pub fn new(buf: &'a mut [MemoryMapEntry]) -> Map<'a> {
        Map { buf, len: 0 }
    }

    pub fn entries(&self) -> &[MemoryMapEntry] {
        &self.buf[..self.len]
    }

    fn insert(&mut self, at: usize, r: Region) -> Result<(), Error> {
        if self.len == self.buf.len() {
            return Err(Error::Full);
        }
        self.buf.copy_within(at..self.len, at + 1);
        self.buf[at] = entry(r);
        self.len += 1;
        Ok(())
    }

    fn remove(&mut self, at: usize) {
        self.buf.copy_within(at + 1..self.len, at);
        self.len -= 1;
    }

    /// Adds a range from the firmware memory map, with its UEFI type and
    /// attributes. Empty ranges are skipped.
    pub fn push_firmware(
        &mut self,
        start: u64,
        pages: u64,
        uefi_type: u32,
        attribute: u64,
    ) -> Result<(), Error> {
        if pages == 0 {
            return Ok(());
        }
        let len = self.len;
        self.insert(
            len,
            Region {
                start,
                pages,
                kind: firmware_kind(uefi_type, attribute),
            },
        )
    }

    /// Sorts the firmware ranges and checks that they do not overlap. Call
    /// once after the last `push_firmware`.
    pub fn sort(&mut self) -> Result<(), Error> {
        self.buf[..self.len].sort_unstable_by_key(|e| e.phys_start);
        for pair in self.entries().windows(2) {
            if region(&pair[0]).end() > pair[1].phys_start {
                return Err(Error::Overlap(pair[1].phys_start));
            }
        }
        Ok(())
    }

    /// Gives `r` its kind, replacing the parts of other ranges it covers and
    /// adding it where no range covered it.
    pub fn overlay(&mut self, r: Region) -> Result<(), Error> {
        let mut i = 0;
        while i < self.len {
            let e = region(&self.buf[i]);
            if e.end() <= r.start || e.start >= r.end() {
                i += 1;
                continue;
            }
            self.remove(i);
            if e.start < r.start {
                let pages = (r.start - e.start) / PAGE_SIZE;
                self.insert(i, Region { pages, ..e })?;
                i += 1;
            }
            if e.end() > r.end() {
                let pages = (e.end() - r.end()) / PAGE_SIZE;
                self.insert(
                    i,
                    Region {
                        start: r.end(),
                        pages,
                        kind: e.kind,
                    },
                )?;
                i += 1;
            }
        }
        let at = self.entries().partition_point(|e| e.phys_start < r.start);
        self.insert(at, r)
    }

    /// Merges adjacent ranges of the same kind.
    pub fn merge(&mut self) {
        let mut i = 1;
        while i < self.len {
            let (prev, cur) = (region(&self.buf[i - 1]), region(&self.buf[i]));
            if prev.kind == cur.kind && prev.end() == cur.start {
                self.buf[i - 1].page_count += cur.pages;
                self.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Fails when a range the direct map covers ends above `limit`.
    /// The end of the highest range that the direct map covers, or 0
    /// without one.
    pub fn direct_map_end(&self) -> u64 {
        self.entries()
            .iter()
            .map(region)
            .filter(|r| r.kind.in_direct_map())
            .map(|r| r.end())
            .max()
            .unwrap_or(0)
    }

    pub fn check_direct_map_limit(&self, limit: u64) -> Result<(), Error> {
        for r in self.entries().iter().map(region) {
            if r.kind.in_direct_map() && r.end() > limit {
                return Err(Error::AboveDirectMapLimit(r.end()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
