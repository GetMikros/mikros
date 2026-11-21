//! The names in an ELF64 image's dynamic section: the libraries it names
//! in `DT_NEEDED` and its own `DT_SONAME`, which `pkg pack` writes into a
//! package's `needs` and pkg-config files (MIKSPEC-0101.R29,
//! MIKSPEC-0101.R85) and the install service keeps libraries for
//! (MIKSPEC-0101.R51).

use alloc::string::String;
use alloc::vec::Vec;

/// The names of an image's dynamic section.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Names {
    pub needed: Vec<String>,
    pub soname: Option<String>,
}

const MAGIC: &[u8; 4] = b"\x7fELF";
const CLASS_64: u8 = 2;
const LITTLE_ENDIAN: u8 = 1;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_SONAME: u64 = 14;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A program header: its type, file offset, address and size in the
/// file.
struct Segment {
    kind: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
}

/// The file offset of the address `vaddr`, in a loaded segment.
fn offset_of(segments: &[Segment], vaddr: u64) -> Option<u64> {
    segments
        .iter()
        .filter(|s| s.kind == PT_LOAD)
        .find(|s| vaddr >= s.vaddr && vaddr < s.vaddr + s.filesz)
        .map(|s| s.offset + (vaddr - s.vaddr))
}

/// The names of the dynamic section of the ELF64 little-endian image
/// `file`, or `None` for a file that is not one or has no dynamic
/// section.
pub fn names(file: &[u8]) -> Option<Names> {
    if file.get(..4)? != MAGIC || *file.get(4)? != CLASS_64 || *file.get(5)? != LITTLE_ENDIAN {
        return None;
    }
    let phoff = u64_at(file, 0x20)? as usize;
    let phentsize = u16_at(file, 0x36)? as usize;
    let phnum = u16_at(file, 0x38)? as usize;
    let mut segments = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let at = phoff.checked_add(i.checked_mul(phentsize)?)?;
        segments.push(Segment {
            kind: u32_at(file, at)?,
            offset: u64_at(file, at + 8)?,
            vaddr: u64_at(file, at + 16)?,
            filesz: u64_at(file, at + 32)?,
        });
    }
    let dynamic = segments.iter().find(|s| s.kind == PT_DYNAMIC)?;
    let mut entries = Vec::new();
    let mut at = dynamic.offset as usize;
    let end = (dynamic.offset + dynamic.filesz) as usize;
    while at + 16 <= end {
        let tag = u64_at(file, at)?;
        if tag == DT_NULL {
            break;
        }
        entries.push((tag, u64_at(file, at + 8)?));
        at += 16;
    }
    let value = |tag| entries.iter().find(|(t, _)| *t == tag).map(|(_, v)| *v);
    let strtab = offset_of(&segments, value(DT_STRTAB)?)? as usize;
    let strsz = value(DT_STRSZ)? as usize;
    let strings = file.get(strtab..strtab.checked_add(strsz)?)?;
    let string = |offset: u64| {
        let rest = strings.get(offset as usize..)?;
        let len = rest.iter().position(|&b| b == 0)?;
        String::from_utf8(rest[..len].to_vec()).ok()
    };
    let mut names = Names::default();
    for (tag, v) in &entries {
        match *tag {
            DT_NEEDED => names.needed.push(string(*v)?),
            DT_SONAME => names.soname = Some(string(*v)?),
            _ => {}
        }
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ELF64 image with one loaded segment that holds its dynamic
    /// section, which names `needed` and `soname`.
    pub fn image(needed: &[&str], soname: Option<&str>) -> Vec<u8> {
        let mut strings = alloc::vec![0u8];
        let mut add = |s: &str| {
            let at = strings.len() as u64;
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
            at
        };
        let mut dynamic: Vec<(u64, u64)> = needed.iter().map(|n| (DT_NEEDED, add(n))).collect();
        if let Some(soname) = soname {
            dynamic.push((DT_SONAME, add(soname)));
        }
        // The header, two program headers, the dynamic section, the strings.
        let phoff = 64u64;
        let dyn_at = phoff + 2 * 56;
        let dyn_len = (dynamic.len() as u64 + 3) * 16;
        let str_at = dyn_at + dyn_len;
        let vaddr = 0x1000;
        dynamic.push((DT_STRTAB, vaddr + str_at));
        dynamic.push((DT_STRSZ, strings.len() as u64));
        dynamic.push((DT_NULL, 0));
        let total = str_at + strings.len() as u64;
        let mut f = alloc::vec![0u8; 64];
        f[..4].copy_from_slice(MAGIC);
        f[4] = CLASS_64;
        f[5] = LITTLE_ENDIAN;
        f[0x20..0x28].copy_from_slice(&phoff.to_le_bytes());
        f[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        f[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
        for (kind, offset, size) in [(PT_LOAD, 0, total), (PT_DYNAMIC, dyn_at, dyn_len)] {
            let mut ph = alloc::vec![0u8; 56];
            ph[..4].copy_from_slice(&kind.to_le_bytes());
            ph[8..16].copy_from_slice(&offset.to_le_bytes());
            ph[16..24].copy_from_slice(&(vaddr + offset).to_le_bytes());
            ph[32..40].copy_from_slice(&size.to_le_bytes());
            f.extend(ph);
        }
        for (tag, value) in dynamic {
            f.extend(tag.to_le_bytes());
            f.extend(value.to_le_bytes());
        }
        f.extend(strings);
        f
    }

    #[test]
    fn reads_the_needed_libraries_and_the_soname() {
        let file = image(
            &["org.glfw.dev:3/glfw", "libfreetype.6.dlib"],
            Some("org.example.lib:x"),
        );
        assert_eq!(
            names(&file),
            Some(Names {
                needed: alloc::vec!["org.glfw.dev:3/glfw".into(), "libfreetype.6.dlib".into()],
                soname: Some("org.example.lib:x".into()),
            })
        );
        assert_eq!(names(b"#!/system/bin/msh\n"), None);
        assert_eq!(names(&file[..70]), None);
    }
}
