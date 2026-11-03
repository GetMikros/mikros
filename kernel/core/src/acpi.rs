//! Finding and checking the ACPI tables the kernel reads.

use core::fmt;

pub mod dmar;
pub mod madt;

/// Reads physical memory.
pub trait PhysRead {
    /// The `len` bytes at `phys`, or `None` when they are not readable.
    fn read(&self, phys: u64, len: usize) -> Option<&[u8]>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes at this address could not be read.
    Unreadable(u64),
    BadRsdp,
    /// A table with this signature has a bad signature, length or checksum.
    BadTable([u8; 4]),
    /// A required table is missing.
    Missing([u8; 4]),
}

fn name(signature: &[u8; 4]) -> &str {
    core::str::from_utf8(signature).unwrap_or("????")
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unreadable(at) => write!(f, "ACPI data at {at:#x} is not in mapped memory"),
            Error::BadRsdp => write!(f, "the ACPI RSDP is invalid"),
            Error::BadTable(s) => write!(f, "the ACPI {} table is invalid", name(s)),
            Error::Missing(s) => write!(f, "the ACPI {} table is missing", name(s)),
        }
    }
}

const HEADER_SIZE: usize = 36;
const RSDP_SIZE: usize = 36;

fn sums_to_zero(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Reads and checks the table at `phys`, which must have `signature`.
fn table(mem: &impl PhysRead, phys: u64, signature: [u8; 4]) -> Result<&[u8], Error> {
    let header = mem.read(phys, HEADER_SIZE).ok_or(Error::Unreadable(phys))?;
    if header[..4] != signature {
        return Err(Error::BadTable(signature));
    }
    let len = u32_at(header, 4).unwrap_or(0) as usize;
    if len < HEADER_SIZE {
        return Err(Error::BadTable(signature));
    }
    let bytes = mem.read(phys, len).ok_or(Error::Unreadable(phys))?;
    if !sums_to_zero(bytes) {
        return Err(Error::BadTable(signature));
    }
    Ok(bytes)
}

/// The tables listed by the XSDT.
pub struct Tables<'m, M: PhysRead> {
    mem: &'m M,
    xsdt: &'m [u8],
}

impl<'m, M: PhysRead> Tables<'m, M> {
    /// Checks the RSDP at `rsdp` and the XSDT it points to.
    pub fn new(mem: &'m M, rsdp: u64) -> Result<Self, Error> {
        let head = mem.read(rsdp, RSDP_SIZE).ok_or(Error::Unreadable(rsdp))?;
        let len = u32_at(head, 20).unwrap_or(0) as usize;
        if &head[..8] != b"RSD PTR " || head[15] < 2 || len < RSDP_SIZE {
            return Err(Error::BadRsdp);
        }
        let bytes = mem.read(rsdp, len).ok_or(Error::Unreadable(rsdp))?;
        if !sums_to_zero(&bytes[..20]) || !sums_to_zero(bytes) {
            return Err(Error::BadRsdp);
        }
        let xsdt_phys = u64_at(bytes, 24).unwrap_or(0);
        let xsdt = table(mem, xsdt_phys, *b"XSDT")?;
        Ok(Tables { mem, xsdt })
    }

    /// The table with `signature`, checked, or `None` when the XSDT lists
    /// none. Only the first one listed is used.
    pub fn find(&self, signature: [u8; 4]) -> Result<Option<&'m [u8]>, Error> {
        let entries = &self.xsdt[HEADER_SIZE..];
        for entry in entries.as_chunks::<8>().0 {
            let phys = u64::from_le_bytes(*entry);
            let header = self.mem.read(phys, 4).ok_or(Error::Unreadable(phys))?;
            if header == signature {
                return table(self.mem, phys, signature).map(Some);
            }
        }
        Ok(None)
    }
}

/// A PCI segment's configuration space in memory, from the MCFG table:
/// its base, segment and first and last bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ecam {
    pub base: u64,
    pub segment: u16,
    pub buses: (u8, u8),
}

impl Ecam {
    /// The physical address of `offset` in the configuration space of a
    /// function, or `None` when the bus is not in this range.
    pub fn address(&self, bus: u8, device: u8, function: u8, offset: u16) -> Option<u64> {
        if !(self.buses.0..=self.buses.1).contains(&bus) || device > 31 || function > 7 {
            return None;
        }
        let at = u64::from(bus - self.buses.0) << 20
            | u64::from(device) << 15
            | u64::from(function) << 12
            | u64::from(offset & 0xFFF);
        Some(self.base + at)
    }
}

/// The configuration space ranges of the MCFG table `mcfg`.
pub fn ecams(mcfg: &[u8]) -> impl Iterator<Item = Ecam> + '_ {
    mcfg.get(HEADER_SIZE + 8..)
        .unwrap_or(&[])
        .as_chunks::<16>()
        .0
        .iter()
        .map(|e| Ecam {
            base: u64::from_le_bytes(e[0..8].try_into().expect("8 bytes")),
            segment: u16::from_le_bytes([e[8], e[9]]),
            buses: (e[10], e[11]),
        })
}

/// Where a register lives, from an ACPI generic address structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    Memory(u64),
    Io(u16),
}

fn generic_address(b: &[u8], at: usize) -> Option<Address> {
    let space = *b.get(at)?;
    let address = u64_at(b, at + 4)?;
    match (space, address) {
        (_, 0) => None,
        (0, a) => Some(Address::Memory(a)),
        (1, a) => u16::try_from(a).ok().map(Address::Io),
        _ => None,
    }
}

/// The ACPI power management timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PmTimer {
    pub address: Address,
    /// Whether the counter is 32 bits wide rather than 24.
    pub wide: bool,
}

/// The PM timer the FADT describes, if any. The extended address is used
/// when it is set.
pub fn pm_timer(fadt: &[u8]) -> Option<PmTimer> {
    const PM_TMR_BLK: usize = 76;
    const PM_TMR_LEN: usize = 91;
    const FLAGS: usize = 112;
    const X_PM_TMR_BLK: usize = 208;
    const TMR_VAL_EXT: u32 = 1 << 8;
    let wide = u32_at(fadt, FLAGS).is_some_and(|f| f & TMR_VAL_EXT != 0);
    let extended = generic_address(fadt, X_PM_TMR_BLK).filter(|_| fadt.len() >= X_PM_TMR_BLK + 12);
    let legacy = || {
        let port = u32_at(fadt, PM_TMR_BLK)?;
        let len = *fadt.get(PM_TMR_LEN)?;
        (port != 0 && len == 4)
            .then(|| u16::try_from(port).ok().map(Address::Io))
            .flatten()
    };
    let address = extended.or_else(legacy)?;
    Some(PmTimer { address, wide })
}

/// The base address of the HPET's registers from the HPET table.
pub fn hpet_base(hpet: &[u8]) -> Option<u64> {
    match generic_address(hpet, 40)? {
        Address::Memory(base) => Some(base),
        Address::Io(_) => None,
    }
}

#[cfg(test)]
mod tests;
