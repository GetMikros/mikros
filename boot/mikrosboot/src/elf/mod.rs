//! Checking the kernel's ELF image.

use alloc::vec::Vec;
use boot_protocol::{NOTE_NAME, NOTE_TYPE, PAGE_SIZE, VERSION_MAJOR};
use core::fmt;

const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const PT_TLS: u32 = 7;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;

/// A loadable segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub mem_size: u64,
    pub offset: u64,
    pub file_size: u64,
    pub flags: u32,
}

impl Segment {
    /// The page-aligned virtual range the segment covers, as start and end.
    /// The end can be 2^64, so both are `u128`.
    pub fn pages(&self) -> (u128, u128) {
        let page = u128::from(PAGE_SIZE);
        let start = u128::from(self.vaddr) / page * page;
        let end = (u128::from(self.vaddr) + u128::from(self.mem_size)).next_multiple_of(page);
        (start, end)
    }
}

/// A kernel image that passed the checks.
#[derive(Debug, PartialEq, Eq)]
pub struct Kernel {
    pub entry: u64,
    pub segments: Vec<Segment>,
    /// The protocol version the kernel was built for.
    pub version: (u16, u16),
}

/// Why a kernel image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    BadHeader,
    Truncated,
    ForbiddenSegment(u32),
    NoLoadSegment,
    BadSegment(usize),
    Misaligned(usize),
    OutsideKernelRange(usize),
    Overlap(usize, usize),
    WritableAndExecutable(usize),
    EntryNotExecutable,
    NoVersionNote,
    SeveralVersionNotes,
    WrongVersion(u16, u16),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::BadHeader => write!(f, "not an x86_64 ELF64 executable"),
            Error::Truncated => write!(f, "the file is truncated"),
            Error::ForbiddenSegment(t) => write!(f, "it has a program header of type {t}"),
            Error::NoLoadSegment => write!(f, "it has no loadable segment"),
            Error::BadSegment(i) => write!(f, "segment {i} has bad sizes or file range"),
            Error::Misaligned(i) => {
                write!(f, "segment {i}'s address and offset differ in page offset")
            }
            Error::OutsideKernelRange(i) => write!(f, "segment {i} is outside the kernel range"),
            Error::Overlap(a, b) => write!(f, "segments {a} and {b} overlap"),
            Error::WritableAndExecutable(i) => write!(f, "segment {i} is writable and executable"),
            Error::EntryNotExecutable => {
                write!(f, "the entry point is not in an executable segment")
            }
            Error::NoVersionNote => write!(f, "it has no Mikros boot protocol note"),
            Error::SeveralVersionNotes => {
                write!(f, "it has more than one Mikros boot protocol note")
            }
            Error::WrongVersion(major, minor) => write!(
                f,
                "it was built for boot protocol {major}.{minor}, not {VERSION_MAJOR}.x"
            ),
        }
    }
}

/// `a + b`, or `Truncated` when it overflows, as an offset that large is
/// past the end of any file.
fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b).ok_or(Error::Truncated)
}

fn u16_at(data: &[u8], offset: u64) -> Result<u16, Error> {
    let offset = usize::try_from(offset).map_err(|_| Error::Truncated)?;
    let end = offset.checked_add(2).ok_or(Error::Truncated)?;
    let bytes = data.get(offset..end).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: u64) -> Result<u32, Error> {
    Ok(u32::from(u16_at(data, offset)?) | u32::from(u16_at(data, add(offset, 2)?)?) << 16)
}

fn u64_at(data: &[u8], offset: u64) -> Result<u64, Error> {
    Ok(u64::from(u32_at(data, offset)?) | u64::from(u32_at(data, add(offset, 4)?)?) << 32)
}

/// The Mikros notes in the note segment at `offset` of `size` bytes, as the
/// version each one gives, or `None` for a note too short to give one.
fn notes(
    file: &[u8],
    offset: u64,
    size: u64,
    align: u64,
) -> Result<Vec<Option<(u16, u16)>>, Error> {
    let align = if align == 8 { 8 } else { 4 };
    let end = offset.checked_add(size).ok_or(Error::Truncated)?;
    let mut found = Vec::new();
    let mut at = offset;
    while add(at, 12)? <= end {
        let name_size = u64::from(u32_at(file, at)?);
        let desc_size = u64::from(u32_at(file, add(at, 4)?)?);
        let kind = u32_at(file, add(at, 8)?)?;
        let name_at = add(at, 12)?;
        let desc_at = add(name_at, name_size.next_multiple_of(align))?;
        let next = add(desc_at, desc_size.next_multiple_of(align))?;
        if next > end || next as usize > file.len() {
            return Err(Error::Truncated);
        }
        let name = &file[name_at as usize..(name_at + name_size) as usize];
        if kind == NOTE_TYPE && name.strip_suffix(b"\0").unwrap_or(name) == NOTE_NAME {
            found.push(if desc_size >= 4 {
                Some((u16_at(file, desc_at)?, u16_at(file, add(desc_at, 2)?)?))
            } else {
                None
            });
        }
        at = next;
    }
    Ok(found)
}

/// Checks the kernel image `file` and returns its entry point, loadable
/// segments and protocol version. Segments must lie at or above
/// `range_start`.
pub fn check(file: &[u8], range_start: u64) -> Result<Kernel, Error> {
    let ident_ok = file.get(..7) == Some(&[0x7f, b'E', b'L', b'F', 2, 1, 1][..]);
    if !ident_ok
        || u16_at(file, 16)? != ET_EXEC
        || u16_at(file, 18)? != EM_X86_64
        || u32_at(file, 20)? != 1
    {
        return Err(Error::BadHeader);
    }
    let entry = u64_at(file, 24)?;
    let ph_offset = u64_at(file, 32)?;
    let ph_size = u64::from(u16_at(file, 54)?);
    let ph_count = u64::from(u16_at(file, 56)?);
    if ph_size < 56 {
        return Err(Error::BadHeader);
    }

    let mut segments: Vec<Segment> = Vec::new();
    let mut versions = Vec::new();
    for i in 0..ph_count {
        let ph = add(ph_offset, i * ph_size)?;
        let kind = u32_at(file, ph)?;
        let flags = u32_at(file, add(ph, 4)?)?;
        let offset = u64_at(file, add(ph, 8)?)?;
        let vaddr = u64_at(file, add(ph, 16)?)?;
        let file_size = u64_at(file, add(ph, 32)?)?;
        let mem_size = u64_at(file, add(ph, 40)?)?;
        let align = u64_at(file, add(ph, 48)?)?;
        match kind {
            PT_INTERP | PT_DYNAMIC | PT_TLS => return Err(Error::ForbiddenSegment(kind)),
            PT_NOTE => versions.extend(notes(file, offset, file_size, align)?),
            PT_LOAD => {
                let index = segments.len();
                let in_file = offset
                    .checked_add(file_size)
                    .is_some_and(|e| e <= file.len() as u64);
                if file_size > mem_size || !in_file {
                    return Err(Error::BadSegment(index));
                }
                if vaddr % PAGE_SIZE != offset % PAGE_SIZE {
                    return Err(Error::Misaligned(index));
                }
                // The kernel range runs to the top of the address space.
                let end = u128::from(vaddr) + u128::from(mem_size);
                if vaddr < range_start || end > 1 << 64 {
                    return Err(Error::OutsideKernelRange(index));
                }
                if flags & PF_W != 0 && flags & PF_X != 0 {
                    return Err(Error::WritableAndExecutable(index));
                }
                segments.push(Segment {
                    vaddr,
                    mem_size,
                    offset,
                    file_size,
                    flags,
                });
            }
            _ => {}
        }
    }
    if segments.is_empty() {
        return Err(Error::NoLoadSegment);
    }
    for (i, a) in segments.iter().enumerate() {
        for (j, b) in segments.iter().enumerate().skip(i + 1) {
            let (a_start, a_end) = a.pages();
            let (b_start, b_end) = b.pages();
            if a_start < b_end && b_start < a_end {
                return Err(Error::Overlap(i, j));
            }
        }
    }
    let executable = segments
        .iter()
        .any(|s| s.flags & PF_X != 0 && s.vaddr <= entry && entry - s.vaddr < s.mem_size);
    if !executable {
        return Err(Error::EntryNotExecutable);
    }

    let version = match versions[..] {
        [] | [None] => return Err(Error::NoVersionNote),
        [Some(version)] => version,
        _ => return Err(Error::SeveralVersionNotes),
    };
    if version.0 != VERSION_MAJOR {
        return Err(Error::WrongVersion(version.0, version.1));
    }
    Ok(Kernel {
        entry,
        segments,
        version,
    })
}

#[cfg(test)]
mod tests;
