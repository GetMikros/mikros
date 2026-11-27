//! What xtask reads of the ELF files of libraries: their `DT_SONAME` and
//! `DT_NEEDED` entries, the symbols of their dynamic symbol tables and the ABI
//! levels that they record (MIKSPEC-0062.R19, MIKSPEC-0068.R22).

use anyhow::{Context, Result, bail};

/// The dynamic entries of an ELF file that name libraries.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dynamic {
    /// Its `DT_SONAME`, if it has one.
    pub soname: Option<String>,
    /// Its `DT_NEEDED` entries, in order.
    pub needed: Vec<String>,
}

/// The names in the dynamic symbol table of an ELF file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Symbols {
    /// The symbols that it defines.
    pub defined: Vec<String>,
    /// The symbols that it needs from elsewhere and that are not weak.
    pub undefined: Vec<String>,
}

/// The `DT_SONAME` and the `DT_NEEDED` entries of the ELF file `data`.
pub fn dynamic(data: &[u8]) -> Result<Dynamic> {
    use object::read::elf::{Dyn, FileHeader, SectionHeader};
    let header = object::elf::FileHeader64::<object::Endianness>::parse(data)
        .map_err(|_| anyhow::anyhow!("is no ELF file"))?;
    let endian = header.endian()?;
    let sections = header.sections(endian, data)?;
    let mut found = Dynamic::default();
    for section in sections.iter() {
        let Some((entries, link)) = section.dynamic(endian, data)? else {
            continue;
        };
        let strings = sections.strings(endian, data, link)?;
        for entry in entries {
            let tag = entry.d_tag(endian);
            if tag != object::elf::DT_SONAME && tag != object::elf::DT_NEEDED {
                continue;
            }
            let name = strings
                .get(entry.d_val(endian) as u32)
                .map_err(|()| anyhow::anyhow!("has a bad dynamic string"))?;
            let name = String::from_utf8_lossy(name).into_owned();
            if tag == object::elf::DT_SONAME {
                found.soname = Some(name);
            } else {
                found.needed.push(name);
            }
        }
    }
    Ok(found)
}

/// The defined symbols and the undefined symbols that are not weak of the
/// dynamic symbol table of the ELF file `data`.
pub fn dynamic_symbols(data: &[u8]) -> Result<Symbols> {
    use object::read::elf::{FileHeader, Sym};
    let header = object::elf::FileHeader64::<object::Endianness>::parse(data)
        .map_err(|_| anyhow::anyhow!("is no ELF file"))?;
    let endian = header.endian()?;
    let sections = header.sections(endian, data)?;
    let mut found = Symbols::default();
    let Some(table) = sections
        .symbols(endian, data, object::elf::SHT_DYNSYM)
        .ok()
        .filter(|t| !t.is_empty())
    else {
        return Ok(found);
    };
    for symbol in table.iter() {
        let name = symbol
            .name(endian, table.strings())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .map_err(|_| anyhow::anyhow!("has a bad symbol name"))?;
        if name.is_empty() {
            continue;
        }
        if symbol.st_shndx(endian) != object::elf::SHN_UNDEF {
            found.defined.push(name);
        } else if symbol.st_bind() != object::elf::STB_WEAK {
            found.undefined.push(name);
        }
    }
    Ok(found)
}

/// The minimum and target ABI levels that the ELF file `data` records, in a
/// note of the name `Mikros` and the type 1, whose target level is not below
/// its minimum.
pub fn abi_levels(data: &[u8]) -> Result<(u32, u32)> {
    use object::read::elf::{FileHeader, ProgramHeader};
    let header = object::elf::FileHeader64::<object::Endianness>::parse(data)
        .map_err(|_| anyhow::anyhow!("is no ELF file, so it has no ABI note"))?;
    let endian = header.endian()?;
    for segment in header.program_headers(endian, data)? {
        let Some(notes) = segment.notes(endian, data)? else {
            continue;
        };
        for note in notes {
            let note = note?;
            if note.name() == b"Mikros" && note.n_type(endian) == object::elf::NoteType(1) {
                let desc = note.desc();
                let (Some(minimum), Some(target), 8) = (
                    desc.get(0..4).and_then(|b| b.try_into().ok()),
                    desc.get(4..8).and_then(|b| b.try_into().ok()),
                    desc.len(),
                ) else {
                    bail!("has an ABI note that is not two 32-bit levels");
                };
                let (minimum, target) = (u32::from_le_bytes(minimum), u32::from_le_bytes(target));
                if target < minimum {
                    bail!(
                        "has an ABI note whose target level {target} is below its minimum {minimum}"
                    );
                }
                return Ok((minimum, target));
            }
        }
    }
    bail!("has no ABI note (name Mikros, type 1)")
}

/// The `DT_SONAME` of the ELF file at `path`, if it has one.
pub fn soname(path: &std::path::Path) -> Result<Option<String>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(dynamic(&data)
        .with_context(|| format!("{} is no ELF file", path.display()))?
        .soname)
}

/// ELF files for the tests of this module and the modules that read ELF
/// files.
#[cfg(test)]
pub mod fixtures {
    /// A little-endian ELF64 file of one `PT_NOTE` segment, which holds the
    /// ABI note of the levels `abi` if it has them.
    pub fn note_elf(abi: Option<(u32, u32)>) -> Vec<u8> {
        let mut note = Vec::new();
        if let Some((minimum, target)) = abi {
            for word in [7u32, 8, 1] {
                note.extend(word.to_le_bytes());
            }
            note.extend(b"Mikros\0\0");
            note.extend(minimum.to_le_bytes());
            note.extend(target.to_le_bytes());
        }
        let mut file = vec![0x7f, b'E', b'L', b'F', 2, 1, 1];
        file.resize(16, 0);
        file.extend(3u16.to_le_bytes()); // e_type
        file.extend(0x3eu16.to_le_bytes()); // e_machine
        file.extend(1u32.to_le_bytes()); // e_version
        file.extend([0u8; 8]); // e_entry
        file.extend(64u64.to_le_bytes()); // e_phoff
        file.extend([0u8; 8]); // e_shoff
        file.extend([0u8; 4]); // e_flags
        file.extend(64u16.to_le_bytes()); // e_ehsize
        file.extend(56u16.to_le_bytes()); // e_phentsize
        file.extend(1u16.to_le_bytes()); // e_phnum
        file.extend(64u16.to_le_bytes()); // e_shentsize
        file.extend([0u8; 4]); // e_shnum, e_shstrndx
        file.extend(4u32.to_le_bytes()); // p_type: PT_NOTE
        file.extend(4u32.to_le_bytes()); // p_flags
        file.extend(120u64.to_le_bytes()); // p_offset
        file.extend([0u8; 16]); // p_vaddr, p_paddr
        file.extend((note.len() as u64).to_le_bytes()); // p_filesz
        file.extend((note.len() as u64).to_le_bytes()); // p_memsz
        file.extend(4u64.to_le_bytes()); // p_align
        file.extend(note);
        file
    }

    /// A symbol of `dynamic_elf`: its name, whether it is defined and whether
    /// it is weak.
    pub struct Symbol(pub &'static str, pub bool, pub bool);

    /// A little-endian ELF64 shared library of the sections `.dynstr`,
    /// `.dynamic` and `.dynsym`, with the soname `soname`, the needed
    /// libraries `needed` and the symbols `symbols`, and the ABI note of
    /// `abi` in a `PT_NOTE` segment when it has one.
    pub fn dynamic_elf(
        soname: Option<&str>,
        needed: &[&str],
        symbols: &[Symbol],
        abi: Option<(u32, u32)>,
    ) -> Vec<u8> {
        // The string table: an empty string, then each name.
        let mut strings = vec![0u8];
        let add = |s: &str, strings: &mut Vec<u8>| {
            let at = strings.len() as u64;
            strings.extend(s.as_bytes());
            strings.push(0);
            at
        };
        let mut dynamic = Vec::new();
        let entry = |tag: u64, value: u64, dynamic: &mut Vec<u8>| {
            dynamic.extend(tag.to_le_bytes());
            dynamic.extend(value.to_le_bytes());
        };
        for name in needed {
            let at = add(name, &mut strings);
            entry(1, at, &mut dynamic); // DT_NEEDED
        }
        if let Some(name) = soname {
            let at = add(name, &mut strings);
            entry(14, at, &mut dynamic); // DT_SONAME
        }
        entry(0, 0, &mut dynamic); // DT_NULL
        let mut table = vec![0u8; 24]; // the null symbol
        for Symbol(name, defined, weak) in symbols {
            let at = add(name, &mut strings);
            table.extend((at as u32).to_le_bytes());
            let bind: u8 = if *weak { 2 } else { 1 };
            table.push(bind << 4 | 1); // st_info: the bind, STT_OBJECT
            table.push(0); // st_other
            table.extend((if *defined { 1u16 } else { 0 }).to_le_bytes()); // st_shndx
            table.extend([0u8; 16]); // st_value, st_size
        }
        let mut note = Vec::new();
        if let Some((minimum, target)) = abi {
            for word in [7u32, 8, 1] {
                note.extend(word.to_le_bytes());
            }
            note.extend(b"Mikros\0\0");
            note.extend(minimum.to_le_bytes());
            note.extend(target.to_le_bytes());
        }
        let phnum: u16 = if abi.is_some() { 1 } else { 0 };
        let header = 64usize;
        let headers = 56 * usize::from(phnum);
        let note_at = header + headers;
        let strings_at = note_at + note.len();
        let dynamic_at = strings_at + strings.len();
        let table_at = dynamic_at + dynamic.len();
        let sections_at = table_at + table.len();
        let mut file = vec![0x7f, b'E', b'L', b'F', 2, 1, 1];
        file.resize(16, 0);
        file.extend(3u16.to_le_bytes()); // e_type
        file.extend(0x3eu16.to_le_bytes()); // e_machine
        file.extend(1u32.to_le_bytes()); // e_version
        file.extend([0u8; 8]); // e_entry
        file.extend((if phnum > 0 { 64u64 } else { 0 }).to_le_bytes()); // e_phoff
        file.extend((sections_at as u64).to_le_bytes()); // e_shoff
        file.extend([0u8; 4]); // e_flags
        file.extend(64u16.to_le_bytes()); // e_ehsize
        file.extend(56u16.to_le_bytes()); // e_phentsize
        file.extend(phnum.to_le_bytes()); // e_phnum
        file.extend(64u16.to_le_bytes()); // e_shentsize
        file.extend(4u16.to_le_bytes()); // e_shnum
        file.extend(1u16.to_le_bytes()); // e_shstrndx: `.dynstr`, whose first byte is empty
        if phnum > 0 {
            file.extend(4u32.to_le_bytes()); // p_type: PT_NOTE
            file.extend(4u32.to_le_bytes()); // p_flags
            file.extend((note_at as u64).to_le_bytes()); // p_offset
            file.extend([0u8; 16]); // p_vaddr, p_paddr
            file.extend((note.len() as u64).to_le_bytes()); // p_filesz
            file.extend((note.len() as u64).to_le_bytes()); // p_memsz
            file.extend(4u64.to_le_bytes()); // p_align
        }
        file.extend(&note);
        file.extend(&strings);
        file.extend(&dynamic);
        file.extend(&table);
        // The section headers: the null one, `.dynstr`, `.dynamic`, `.dynsym`.
        file.extend([0u8; 64]);
        let section = |kind: u32, at: usize, size: usize, link: u32, info: u32, entsize: u64| {
            let mut header = Vec::new();
            header.extend(0u32.to_le_bytes()); // sh_name
            header.extend(kind.to_le_bytes());
            header.extend(2u64.to_le_bytes()); // sh_flags: SHF_ALLOC
            header.extend(0u64.to_le_bytes()); // sh_addr
            header.extend((at as u64).to_le_bytes());
            header.extend((size as u64).to_le_bytes());
            header.extend(link.to_le_bytes());
            header.extend(info.to_le_bytes());
            header.extend(8u64.to_le_bytes()); // sh_addralign
            header.extend(entsize.to_le_bytes());
            header
        };
        file.extend(section(3, strings_at, strings.len(), 0, 0, 0)); // SHT_STRTAB
        file.extend(section(6, dynamic_at, dynamic.len(), 1, 0, 16)); // SHT_DYNAMIC
        file.extend(section(11, table_at, table.len(), 1, 1, 24)); // SHT_DYNSYM
        file
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{Symbol, dynamic_elf, note_elf};
    use super::*;

    #[test]
    fn reads_the_soname_and_the_needed_libraries() {
        let data = dynamic_elf(
            Some("libx.dlib"),
            &["libc.dlib", "libwayland.dlib"],
            &[],
            None,
        );
        let found = dynamic(&data).unwrap();
        assert_eq!(found.soname.as_deref(), Some("libx.dlib"));
        assert_eq!(found.needed, ["libc.dlib", "libwayland.dlib"]);
        let none = dynamic(&dynamic_elf(None, &[], &[], None)).unwrap();
        assert_eq!(none, Dynamic::default());
        assert!(dynamic(b"#!/bin/msh\n").is_err());
    }

    #[test]
    fn separates_the_defined_symbols_from_the_undefined_ones_that_are_not_weak() {
        let data = dynamic_elf(
            Some("libx.dlib"),
            &[],
            &[
                Symbol("defined", true, false),
                Symbol("needed", false, false),
                Symbol("optional", false, true),
                Symbol("weak_definition", true, true),
            ],
            None,
        );
        let found = dynamic_symbols(&data).unwrap();
        assert_eq!(found.defined, ["defined", "weak_definition"]);
        assert_eq!(found.undefined, ["needed"]);
        let none = dynamic_symbols(&dynamic_elf(None, &[], &[], None)).unwrap();
        assert_eq!(none, Symbols::default());
    }

    #[test]
    fn a_library_records_its_abi_levels() {
        assert_eq!(abi_levels(&note_elf(Some((2, 5)))).unwrap(), (2, 5));
        assert_eq!(
            abi_levels(&dynamic_elf(None, &[], &[], Some((1, 3)))).unwrap(),
            (1, 3)
        );
        let error = |data: &[u8]| format!("{:#}", abi_levels(data).unwrap_err());
        assert!(error(&note_elf(None)).contains("no ABI note"));
        assert!(error(&note_elf(Some((5, 2)))).contains("below its minimum"));
        assert!(error(b"#!/bin/msh\n").contains("no ELF"));
    }
}
