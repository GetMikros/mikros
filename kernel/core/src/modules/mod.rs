//! The boot modules that the bootloader passes, and the index that the
//! boot image memory object starts with.

use alloc::vec;
use alloc::vec::Vec;
use boot_protocol::{Kind, MemoryMapEntry, PAGE_SIZE};
use core::fmt;

#[cfg(test)]
mod tests;

/// The most modules a boot may have.
pub const MODULES_MAX: u64 = 4096;
/// The longest module path.
pub const PATH_MAX: u32 = 1024;
/// The size of an entry of the module array.
pub const ENTRY_SIZE: usize = 32;
/// The magic and version of the index.
pub const INDEX_MAGIC: [u8; 8] = *b"MIKROSBM";
pub const INDEX_VERSION: u32 = 1;
/// The sizes of the index header and of one of its entries.
const INDEX_HEADER: usize = 16;
const INDEX_ENTRY: usize = 24;

/// An entry of the module array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Module {
    pub phys: u64,
    pub size: u64,
    pub path_phys: u64,
    pub path_len: u32,
}

impl Module {
    /// The physical range of the module's pages, as start and end.
    fn pages(&self) -> Option<(u64, u64)> {
        let end = self
            .phys
            .checked_add(self.size)?
            .checked_next_multiple_of(PAGE_SIZE)?;
        Some((self.phys, end))
    }
}

/// Why the module array cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `modules_count` is zero or above `MODULES_MAX`.
    Count(u64),
    /// The array does not lie in `BootloaderReclaimable` memory.
    Array,
    /// The entry of this index has no contents but an address.
    EmptyWithAddress(usize),
    /// The entry of this index has an address that is not page-aligned.
    Unaligned(usize),
    /// The contents of the entry of this index do not lie in `BootImage`
    /// memory.
    NotBootImage(usize),
    /// The contents of the entries of these indices overlap.
    Overlap(usize, usize),
    /// The path of the entry of this index does not lie in
    /// `BootloaderReclaimable` memory.
    PathMemory(usize),
    /// The path of the entry of this index is not a valid module path.
    Path(usize),
    /// The entries of these indices have the same path.
    SamePath(usize, usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Count(n) => write!(f, "the bootloader passed {n} boot modules"),
            Error::Array => write!(f, "the boot module array is not in bootloader memory"),
            Error::EmptyWithAddress(i) => write!(f, "boot module {i} is empty but has an address"),
            Error::Unaligned(i) => write!(f, "boot module {i} is not page-aligned"),
            Error::NotBootImage(i) => write!(f, "boot module {i} is not in boot image memory"),
            Error::Overlap(i, j) => write!(f, "boot modules {i} and {j} overlap"),
            Error::PathMemory(i) => {
                write!(f, "the path of boot module {i} is not in bootloader memory")
            }
            Error::Path(i) => write!(f, "boot module {i} has an invalid path"),
            Error::SamePath(i, j) => write!(f, "boot modules {i} and {j} have the same path"),
        }
    }
}

/// The entries of the module array `bytes`.
pub fn decode(bytes: &[u8]) -> Vec<Module> {
    let word = |e: &[u8], at: usize| u64::from_le_bytes(e[at..at + 8].try_into().expect("8 bytes"));
    bytes
        .as_chunks::<ENTRY_SIZE>()
        .0
        .iter()
        .map(|e| Module {
            phys: word(e, 0),
            size: word(e, 8),
            path_phys: word(e, 16),
            path_len: u32::from_le_bytes(e[24..28].try_into().expect("4 bytes")),
        })
        .collect()
}

/// Whether the `len` bytes from `start` lie in ranges of `kind` of `map`,
/// which may be in any order.
pub fn covered(
    map: impl Iterator<Item = MemoryMapEntry> + Clone,
    kind: Kind,
    start: u64,
    len: u64,
) -> bool {
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    let mut at = start;
    while at < end {
        let next = map
            .clone()
            .filter(|e| e.kind == kind as u32)
            .filter_map(|e| {
                let range_end = e
                    .phys_start
                    .checked_add(e.page_count.checked_mul(PAGE_SIZE)?)?;
                (e.phys_start <= at && at < range_end).then_some(range_end)
            })
            .max();
        match next {
            Some(next) => at = next,
            None => return false,
        }
    }
    true
}

/// Whether `path` has the form of a module path: UTF-8, 1 to `PATH_MAX`
/// bytes, components separated by `/`, none of them empty, `.` or `..`.
pub fn valid_path(path: &[u8]) -> bool {
    !path.is_empty()
        && path.len() <= PATH_MAX as usize
        && core::str::from_utf8(path).is_ok()
        && path
            .split(|&b| b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b"..")
}

/// Checks the module array at `array_phys` with `count` entries
/// `modules`, against the memory map `map`. `path` reads the path of an
/// entry, which must lie in `BootloaderReclaimable` memory first.
pub fn check<'a>(
    array_phys: u64,
    modules: &[Module],
    map: impl Iterator<Item = MemoryMapEntry> + Clone,
    path: impl Fn(&Module) -> &'a [u8],
) -> Result<(), Error> {
    let count = modules.len() as u64;
    if count == 0 || count > MODULES_MAX {
        return Err(Error::Count(count));
    }
    if !covered(
        map.clone(),
        Kind::BootloaderReclaimable,
        array_phys,
        count * ENTRY_SIZE as u64,
    ) {
        return Err(Error::Array);
    }
    let mut ranges = Vec::new();
    for (i, m) in modules.iter().enumerate() {
        if m.size == 0 {
            if m.phys != 0 {
                return Err(Error::EmptyWithAddress(i));
            }
        } else if m.phys % PAGE_SIZE != 0 {
            return Err(Error::Unaligned(i));
        } else {
            let (start, end) = m.pages().ok_or(Error::NotBootImage(i))?;
            if !covered(map.clone(), Kind::BootImage, start, end - start) {
                return Err(Error::NotBootImage(i));
            }
            ranges.push((start, end, i));
        }
        let reclaimable = covered(
            map.clone(),
            Kind::BootloaderReclaimable,
            m.path_phys,
            u64::from(m.path_len),
        );
        if !reclaimable || m.path_len == 0 {
            return Err(if reclaimable {
                Error::Path(i)
            } else {
                Error::PathMemory(i)
            });
        }
        if !valid_path(path(m)) {
            return Err(Error::Path(i));
        }
    }
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(Error::Overlap(
                pair[0].2.min(pair[1].2),
                pair[0].2.max(pair[1].2),
            ));
        }
    }
    let mut order: Vec<usize> = (0..modules.len()).collect();
    order.sort_unstable_by(|&a, &b| path(&modules[a]).cmp(path(&modules[b])).then(a.cmp(&b)));
    for pair in order.windows(2) {
        if path(&modules[pair[0]]) == path(&modules[pair[1]]) {
            return Err(Error::SamePath(pair[0], pair[1]));
        }
    }
    Ok(())
}

/// Where the boot image memory object holds each module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The index, filling whole pages.
    pub index: Vec<u8>,
    /// The offset of each module's contents, zero for an empty one.
    pub offsets: Vec<u64>,
    /// The size of the object.
    pub size: u64,
}

/// The layout of the boot image object for modules of `sizes` and
/// `paths`, in the order of the module array.
pub fn layout(sizes: &[u64], paths: &[&[u8]]) -> Layout {
    let count = sizes.len();
    let paths_len: usize = paths.iter().map(|p| p.len()).sum();
    let len = INDEX_HEADER + INDEX_ENTRY * count + paths_len;
    let index_size = (len as u64).next_multiple_of(PAGE_SIZE);
    let mut index = vec![0u8; index_size as usize];
    index[..8].copy_from_slice(&INDEX_MAGIC);
    index[8..12].copy_from_slice(&INDEX_VERSION.to_le_bytes());
    index[12..16].copy_from_slice(&(count as u32).to_le_bytes());
    let mut offsets = Vec::with_capacity(count);
    let mut next = index_size;
    let mut path_at = INDEX_HEADER + INDEX_ENTRY * count;
    for (i, (&size, path)) in sizes.iter().zip(paths).enumerate() {
        let offset = if size == 0 { 0 } else { next };
        next += size.next_multiple_of(PAGE_SIZE);
        let e = INDEX_HEADER + INDEX_ENTRY * i;
        index[e..e + 8].copy_from_slice(&offset.to_le_bytes());
        index[e + 8..e + 16].copy_from_slice(&size.to_le_bytes());
        index[e + 16..e + 20].copy_from_slice(&(path_at as u32).to_le_bytes());
        index[e + 20..e + 24].copy_from_slice(&(path.len() as u32).to_le_bytes());
        index[path_at..path_at + path.len()].copy_from_slice(path);
        path_at += path.len();
        offsets.push(offset);
    }
    Layout {
        index,
        offsets,
        size: next,
    }
}
