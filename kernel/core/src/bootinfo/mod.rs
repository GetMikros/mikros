//! Decoding the `BootInfo` structure and the memory map the bootloader
//! passes to the kernel.

use boot_protocol::{
    BOOT_INFO_SIZE, BootInfo, Framebuffer, Kind, MAGIC, MemoryMapEntry, VERSION_MAJOR,
};
use core::fmt;

/// The bytes needed to find the size of `BootInfo`.
pub const HEADER_SIZE: usize = 0x10;

/// The smallest memory map entry size.
pub const MIN_ENTRY_SIZE: u64 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes were given than the structure needs.
    Truncated,
    BadMagic,
    /// The major and minor version.
    UnsupportedVersion(u16, u16),
    /// The size field is smaller than the fields this kernel reads.
    BadSize(u32),
    /// The memory map entry size is below `MIN_ENTRY_SIZE`.
    BadEntrySize(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Truncated => write!(f, "boot information is truncated"),
            Error::BadMagic => write!(f, "boot information has a wrong magic value"),
            Error::UnsupportedVersion(major, minor) => {
                write!(f, "boot protocol {major}.{minor} is not supported")
            }
            Error::BadSize(size) => write!(f, "boot information size {size} is too small"),
            Error::BadEntrySize(size) => write!(f, "memory map entry size {size} is too small"),
        }
    }
}

fn bytes<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N], Error> {
    let slice = data.get(at..at + N).ok_or(Error::Truncated)?;
    let mut out = [0; N];
    out.copy_from_slice(slice);
    Ok(out)
}

fn u16_at(data: &[u8], at: usize) -> Result<u16, Error> {
    bytes(data, at).map(u16::from_le_bytes)
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, Error> {
    bytes(data, at).map(u32::from_le_bytes)
}

fn u64_at(data: &[u8], at: usize) -> Result<u64, Error> {
    bytes(data, at).map(u64::from_le_bytes)
}

/// Checks the header in the first `HEADER_SIZE` bytes and returns the size
/// of the whole structure.
pub fn size(header: &[u8]) -> Result<usize, Error> {
    if bytes::<8>(header, 0)? != MAGIC {
        return Err(Error::BadMagic);
    }
    let major = u16_at(header, 0x08)?;
    let minor = u16_at(header, 0x0A)?;
    if major != VERSION_MAJOR {
        return Err(Error::UnsupportedVersion(major, minor));
    }
    let size = u32_at(header, 0x0C)?;
    if size < BOOT_INFO_SIZE {
        return Err(Error::BadSize(size));
    }
    Ok(size as usize)
}

/// Decodes `data`, which holds the whole structure as `size` gives it.
/// Bytes after the fields of version 1.0 are ignored.
pub fn decode(data: &[u8]) -> Result<BootInfo, Error> {
    let size = size(data)?;
    // Only the fields of version 1.0 are read, all of which lie below the
    // checked size.
    let d = data.get(..size).ok_or(Error::Truncated)?;
    let framebuffer = Framebuffer {
        phys_base: u64_at(d, 0x80)?,
        size: u64_at(d, 0x88)?,
        width: u32_at(d, 0x90)?,
        height: u32_at(d, 0x94)?,
        stride: u32_at(d, 0x98)?,
        format: u32_at(d, 0x9C)?,
        red_mask: u32_at(d, 0xA0)?,
        green_mask: u32_at(d, 0xA4)?,
        blue_mask: u32_at(d, 0xA8)?,
        reserved_mask: u32_at(d, 0xAC)?,
    };
    Ok(BootInfo {
        magic: MAGIC,
        version_major: u16_at(d, 0x08)?,
        version_minor: u16_at(d, 0x0A)?,
        size: u32_at(d, 0x0C)?,
        arch: u32_at(d, 0x10)?,
        flags: u32_at(d, 0x14)?,
        memory_map_phys: u64_at(d, 0x18)?,
        memory_map_count: u64_at(d, 0x20)?,
        memory_map_entry_size: u64_at(d, 0x28)?,
        kernel_phys_base: u64_at(d, 0x30)?,
        kernel_virt_base: u64_at(d, 0x38)?,
        kernel_size: u64_at(d, 0x40)?,
        direct_map_base: u64_at(d, 0x48)?,
        boot_image_phys: u64_at(d, 0x50)?,
        boot_image_size: u64_at(d, 0x58)?,
        cmdline_phys: u64_at(d, 0x60)?,
        cmdline_len: u64_at(d, 0x68)?,
        firmware_table_phys: u64_at(d, 0x70)?,
        uefi_system_table_phys: u64_at(d, 0x78)?,
        framebuffer,
        rng_seed: bytes(d, 0xB0)?,
        boot_time: u64_at(d, 0xD0)? as i64,
        arch_data0: u64_at(d, 0xD8)?,
        arch_data1: u64_at(d, 0xE0)?,
    })
}

/// The size in bytes of a memory map of `count` entries of `entry_size`
/// bytes each, or `None` when it does not fit in the address space.
pub fn memory_map_size(count: u64, entry_size: u64) -> Result<Option<usize>, Error> {
    if entry_size < MIN_ENTRY_SIZE {
        return Err(Error::BadEntrySize(entry_size));
    }
    Ok(count
        .checked_mul(entry_size)
        .and_then(|n| usize::try_from(n).ok()))
}

/// The entries of the memory map in `data`, which is `entry_size` bytes per
/// entry. Bytes of an entry after its first `MIN_ENTRY_SIZE` are ignored,
/// as is a partial entry at the end.
pub fn memory_map(
    data: &[u8],
    entry_size: u64,
) -> Result<impl Iterator<Item = MemoryMapEntry> + Clone + '_, Error> {
    if entry_size < MIN_ENTRY_SIZE {
        return Err(Error::BadEntrySize(entry_size));
    }
    let step = usize::try_from(entry_size).map_err(|_| Error::BadEntrySize(entry_size))?;
    Ok(data.chunks_exact(step).map(|e| MemoryMapEntry {
        phys_start: u64::from_le_bytes(e[0..8].try_into().unwrap_or_default()),
        page_count: u64::from_le_bytes(e[8..16].try_into().unwrap_or_default()),
        kind: u32::from_le_bytes(e[16..20].try_into().unwrap_or_default()),
        reserved: u32::from_le_bytes(e[20..24].try_into().unwrap_or_default()),
    }))
}

/// Why a framebuffer is not used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramebufferError {
    /// Its width or height is zero.
    Empty,
    /// Its stride is less than its width.
    NarrowStride,
    /// Its size is less than its stride times its height times 4 bytes.
    TooSmall,
    /// Its range runs past the end of the physical address space.
    RangeOverflow,
    /// Part of its range is not in a `Framebuffer` range of the memory map.
    NotInMemoryMap,
}

impl fmt::Display for FramebufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FramebufferError::Empty => "the framebuffer has no pixels",
            FramebufferError::NarrowStride => "the framebuffer stride is less than its width",
            FramebufferError::TooSmall => "the framebuffer is smaller than its lines",
            FramebufferError::RangeOverflow => "the framebuffer range overflows",
            FramebufferError::NotInMemoryMap => {
                "the framebuffer is not in Framebuffer memory map ranges"
            }
        })
    }
}

/// Checks the geometry of `fb`, and that its range is covered by the
/// `Framebuffer` ranges of `map`.
pub fn check_framebuffer(
    fb: &Framebuffer,
    map: impl Iterator<Item = MemoryMapEntry> + Clone,
) -> Result<(), FramebufferError> {
    if fb.width == 0 || fb.height == 0 {
        return Err(FramebufferError::Empty);
    }
    if fb.stride < fb.width {
        return Err(FramebufferError::NarrowStride);
    }
    let lines = u64::from(fb.stride)
        .checked_mul(u64::from(fb.height))
        .and_then(|n| n.checked_mul(4));
    if lines.is_none_or(|n| n > fb.size) {
        return Err(FramebufferError::TooSmall);
    }
    let end = fb
        .phys_base
        .checked_add(fb.size)
        .ok_or(FramebufferError::RangeOverflow)?;
    // Moves through the range from range to range of the map, which may
    // be in any order.
    let mut at = fb.phys_base;
    while at < end {
        at = map
            .clone()
            .filter(|e| e.kind == Kind::Framebuffer as u32)
            .filter_map(|e| {
                let len = e.page_count.checked_mul(4096)?;
                let range_end = e.phys_start.checked_add(len)?;
                (e.phys_start <= at && at < range_end).then_some(range_end)
            })
            .max()
            .ok_or(FramebufferError::NotInMemoryMap)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
