//! Checking the ELF image of `init` and finding its segments.

use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

/// The `e_machine` value of x86_64.
pub const EM_X86_64: u16 = 62;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_TLS: u32 = 7;
const ET_EXEC: u16 = 2;
const HEADER_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const PAGE: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file is too short for the ELF header or the program headers.
    Truncated,
    /// The identification, type, machine or version is not the expected
    /// one.
    Unsupported,
    /// A `PT_INTERP`, `PT_DYNAMIC` or `PT_TLS` program header.
    Refused(u32),
    NoLoad,
    /// The `PT_LOAD` program header with this index has its data outside
    /// the file, its pages outside the allowed range, more file data than
    /// memory, or is both writable and executable.
    BadSegment(usize),
    /// The `PT_LOAD` program headers with these indexes share a page.
    Overlap(usize, usize),
    BadEntry(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Truncated => write!(f, "the ELF image is truncated"),
            Error::Unsupported => write!(f, "the ELF image is not a supported executable"),
            Error::Refused(kind) => write!(f, "the ELF image has a program header of type {kind}"),
            Error::NoLoad => write!(f, "the ELF image has no loadable segment"),
            Error::BadSegment(i) => write!(f, "the ELF segment {i} is not allowed"),
            Error::Overlap(a, b) => write!(f, "the ELF segments {a} and {b} overlap"),
            Error::BadEntry(e) => write!(f, "the ELF entry point {e:#x} is not executable"),
        }
    }
}

/// A `PT_LOAD` segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    /// The file data: `filesz` bytes from `offset`.
    pub offset: u64,
    pub filesz: u64,
    pub flags: u32,
}

impl Segment {
    /// The pages the segment covers.
    pub fn pages(&self) -> Range<u64> {
        self.vaddr & !(PAGE - 1)..(self.vaddr + self.memsz).next_multiple_of(PAGE)
    }
}

/// A checked executable.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    pub entry: u64,
    pub segments: Vec<Segment>,
}

fn u16_at(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().expect("8 bytes"))
}

/// Checks `file`, an executable for `machine` whose segments must lie in
/// `user`, and returns its entry point and loadable segments.
pub fn parse(file: &[u8], machine: u16, user: Range<u64>) -> Result<Image, Error> {
    let h = file.get(..HEADER_SIZE).ok_or(Error::Truncated)?;
    let ident_ok = h[..4] == *b"\x7fELF" && h[4] == 2 && h[5] == 1 && h[6] == 1;
    if !ident_ok || u16_at(h, 16) != ET_EXEC || u16_at(h, 18) != machine || u32_at(h, 20) != 1 {
        return Err(Error::Unsupported);
    }
    let entry = u64_at(h, 24);
    let phoff = u64_at(h, 32);
    let (phentsize, phnum) = (u16_at(h, 54) as usize, u16_at(h, 56) as usize);
    if phnum != 0 && phentsize < PHDR_SIZE {
        return Err(Error::Unsupported);
    }
    let table = usize::try_from(phoff)
        .ok()
        .and_then(|start| Some(start..start.checked_add(phentsize * phnum)?))
        .and_then(|r| file.get(r))
        .ok_or(Error::Truncated)?;

    let mut segments: Vec<(usize, Segment)> = Vec::new();
    for (i, p) in table.chunks_exact(phentsize.max(1)).take(phnum).enumerate() {
        let kind = u32_at(p, 0);
        match kind {
            PT_INTERP | PT_DYNAMIC | PT_TLS => return Err(Error::Refused(kind)),
            PT_LOAD => {}
            _ => continue,
        }
        let s = Segment {
            flags: u32_at(p, 4),
            offset: u64_at(p, 8),
            vaddr: u64_at(p, 16),
            filesz: u64_at(p, 32),
            memsz: u64_at(p, 40),
        };
        let in_file = s
            .offset
            .checked_add(s.filesz)
            .is_some_and(|e| e <= file.len() as u64);
        let in_user = s.memsz > 0
            && s.vaddr >= user.start
            && s.vaddr.checked_add(s.memsz).is_some_and(|e| e <= user.end);
        let wx = s.flags & (PF_W | PF_X) == PF_W | PF_X;
        if !in_file || !in_user || s.filesz > s.memsz || wx {
            return Err(Error::BadSegment(i));
        }
        segments.push((i, s));
    }
    if segments.is_empty() {
        return Err(Error::NoLoad);
    }
    for (n, (i, a)) in segments.iter().enumerate() {
        for (j, b) in &segments[n + 1..] {
            let (pa, pb) = (a.pages(), b.pages());
            if pa.start < pb.end && pb.start < pa.end {
                return Err(Error::Overlap(*i, *j));
            }
        }
    }
    let executable =
        |s: &Segment| s.flags & PF_X != 0 && (s.vaddr..s.vaddr + s.memsz).contains(&entry);
    if !segments.iter().any(|(_, s)| executable(s)) {
        return Err(Error::BadEntry(entry));
    }
    Ok(Image {
        entry,
        segments: segments.into_iter().map(|(_, s)| s).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    const USER: Range<u64> = 0x1_0000..0x7FFF_FFFF_F000;

    /// An executable with `segments` as (type, flags, offset, vaddr,
    /// filesz, memsz), its program headers right after the ELF header,
    /// padded with zeros to `len` bytes.
    fn elf(entry: u64, segments: &[(u32, u32, u64, u64, u64, u64)], len: usize) -> Vec<u8> {
        let mut f = vec![0u8; HEADER_SIZE];
        f[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        f[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        f[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        f[20..24].copy_from_slice(&1u32.to_le_bytes());
        f[24..32].copy_from_slice(&entry.to_le_bytes());
        f[32..40].copy_from_slice(&64u64.to_le_bytes());
        f[54..56].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        f[56..58].copy_from_slice(&(segments.len() as u16).to_le_bytes());
        for &(kind, flags, offset, vaddr, filesz, memsz) in segments {
            let mut p = vec![0u8; PHDR_SIZE];
            p[0..4].copy_from_slice(&kind.to_le_bytes());
            p[4..8].copy_from_slice(&flags.to_le_bytes());
            p[8..16].copy_from_slice(&offset.to_le_bytes());
            p[16..24].copy_from_slice(&vaddr.to_le_bytes());
            p[32..40].copy_from_slice(&filesz.to_le_bytes());
            p[40..48].copy_from_slice(&memsz.to_le_bytes());
            f.extend(p);
        }
        f.resize(len.max(f.len()), 0);
        f
    }

    const TEXT: (u32, u32, u64, u64, u64, u64) = (PT_LOAD, PF_X | 4, 0, 0x40_0000, 0x200, 0x200);
    const DATA: (u32, u32, u64, u64, u64, u64) =
        (PT_LOAD, PF_W | 4, 0x200, 0x40_1200, 0x10, 0x3000);

    fn check(entry: u64, segments: &[(u32, u32, u64, u64, u64, u64)]) -> Result<Image, Error> {
        parse(&elf(entry, segments, 0x400), EM_X86_64, USER)
    }

    #[test]
    fn accepts_a_static_executable() {
        let image = check(0x40_0100, &[TEXT, (6, 4, 64, 0x40_0040, 0, 0), DATA]).unwrap();
        assert_eq!(image.entry, 0x40_0100);
        assert_eq!(image.segments.len(), 2);
        assert_eq!(image.segments[1].pages(), 0x40_1000..0x40_5000);
        assert_eq!(image.segments[1].filesz, 0x10);
    }

    #[test]
    fn refuses_other_files() {
        let mut f = elf(0x40_0100, &[TEXT], 0x400);
        assert_eq!(parse(&f[..63], EM_X86_64, USER), Err(Error::Truncated));
        assert_eq!(parse(&f, 3, USER), Err(Error::Unsupported));
        f[16] = 3;
        assert_eq!(parse(&f, EM_X86_64, USER), Err(Error::Unsupported));
        let mut f = elf(0x40_0100, &[TEXT], 0x400);
        f[4] = 1;
        assert_eq!(parse(&f, EM_X86_64, USER), Err(Error::Unsupported));
        let f = elf(0x40_0100, &[TEXT], 0);
        assert_eq!(parse(&f[..100], EM_X86_64, USER), Err(Error::Truncated));
    }

    #[test]
    fn refuses_dynamic_and_tls_images() {
        for kind in [PT_INTERP, PT_DYNAMIC, PT_TLS] {
            let e = check(0x40_0100, &[TEXT, (kind, 4, 0, 0x40_0000, 0, 0)]);
            assert_eq!(e, Err(Error::Refused(kind)));
        }
        assert_eq!(check(0x40_0100, &[]), Err(Error::NoLoad));
    }

    #[test]
    fn checks_segments() {
        let bad = |s| check(0x40_0100, &[TEXT, s]);
        assert_eq!(
            bad((PT_LOAD, 4, 0x300, 0x50_0000, 0x200, 0x200)),
            Err(Error::BadSegment(1))
        );
        assert_eq!(
            bad((PT_LOAD, 4, 0, 0xF000, 0x10, 0x10)),
            Err(Error::BadSegment(1))
        );
        assert_eq!(
            bad((PT_LOAD, 4, 0, USER.end - 8, 0, 0x10)),
            Err(Error::BadSegment(1))
        );
        assert_eq!(
            bad((PT_LOAD, 4, 0, u64::MAX - 8, 0, 0x10)),
            Err(Error::BadSegment(1))
        );
        assert_eq!(
            bad((PT_LOAD, 4, 0, 0x50_0000, 0x20, 0x10)),
            Err(Error::BadSegment(1))
        );
        assert_eq!(
            bad((PT_LOAD, PF_W | PF_X, 0, 0x50_0000, 0, 0x10)),
            Err(Error::BadSegment(1))
        );
        assert_eq!(
            bad((PT_LOAD, 4, 0, 0x40_0FFF, 0, 1)),
            Err(Error::Overlap(0, 1))
        );
        assert!(bad((PT_LOAD, 4, 0, 0x40_1000, 0, 1)).is_ok());
    }

    #[test]
    fn needs_an_executable_entry() {
        assert_eq!(check(0x40_0200, &[TEXT]), Err(Error::BadEntry(0x40_0200)));
        assert_eq!(
            check(0x40_1200, &[TEXT, DATA]),
            Err(Error::BadEntry(0x40_1200))
        );
    }
}
