//! Loading native executables into new processes: checking the image,
//! placing it at an address the kernel chooses, mapping its segments, and
//! giving the main thread its stack.

#![no_std]

use mikros_rt::sys::{self, Handle, Status};

const PAGE_SIZE: u64 = 4096;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PERM_EXECUTE: u32 = 1 << 2;
const SPECIFIC: u32 = 1 << 8;

const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const PHDR_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// The only dynamic linker an image may name.
pub const INTERPRETER: &[u8] = b"/usr/lib/ld-mikros.dlib";
/// The size of the main thread's stack.
pub const STACK_SIZE: u64 = 8 << 20;

/// Why an image was refused or could not be loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a little-endian ELF64 image for x86-64.
    NotElf,
    /// An `ET_EXEC` image, or another type that is not `ET_DYN`.
    NotPositionIndependent,
    /// A program header or segment that lies outside the file.
    OutsideFile,
    /// A loadable segment that is both writable and executable.
    WriteExecute,
    /// Loadable segments that overlap.
    Overlap,
    /// A `PT_INTERP` other than `INTERPRETER`.
    Interpreter,
    /// A segment that cannot be mapped from the file as it is: one whose
    /// file offset and address differ within a page, that is larger in
    /// the file than in memory, or that has no `PF_W` but is larger in
    /// memory than in the file.
    Unmappable,
    /// A system call failed.
    Status(Status),
}

/// An executable file to load.
pub struct Image<'a> {
    /// The bytes of the file.
    pub file: &'a [u8],
    /// A memory object with the file's bytes from its offset 0, which
    /// segments without `PF_W` are mapped from.
    pub shared: Handle,
    /// Returns a new memory object, private to the process, holding the
    /// file's bytes from the page-aligned `offset` for `len` bytes, a
    /// whole number of pages; writable segments are mapped from it.
    pub private: &'a dyn Fn(u64, u64) -> Result<Handle, Error>,
}

/// Where an image was loaded.
#[derive(Clone, Copy, Debug)]
pub struct Loaded<'a> {
    /// What the image's addresses were moved by.
    pub base: u64,
    pub entry: u64,
    /// The address of the program headers in the process.
    pub phdr: u64,
    pub phnum: u32,
    /// The dynamic linker the image names, if any.
    pub interpreter: Option<&'a [u8]>,
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

fn check(status: Status) -> Result<(), Error> {
    if status == mikros_rt::OK {
        Ok(())
    } else {
        Err(Error::Status(status))
    }
}

fn page_down(v: u64) -> u64 {
    v & !(PAGE_SIZE - 1)
}

fn page_up(v: u64) -> Result<u64, Error> {
    v.checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Error::Unmappable)
}

/// A loadable segment.
#[derive(Clone, Copy)]
struct Segment {
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

/// The program headers of `file`, checked to lie inside it.
fn headers(file: &[u8]) -> Result<impl Iterator<Item = &[u8; PHDR_SIZE]>, Error> {
    if file.len() < 64
        || file[..4] != *b"\x7fELF"
        || file[4] != ELFCLASS64
        || file[5] != ELFDATA2LSB
        || u16_at(file, 18) != EM_X86_64
    {
        return Err(Error::NotElf);
    }
    let kind = u16_at(file, 16);
    if kind != ET_DYN {
        return Err(Error::NotPositionIndependent);
    }
    let phoff = u64_at(file, 32);
    let phentsize = usize::from(u16_at(file, 54));
    let phnum = usize::from(u16_at(file, 56));
    if phentsize != PHDR_SIZE {
        return Err(Error::NotElf);
    }
    let start = usize::try_from(phoff).map_err(|_| Error::OutsideFile)?;
    let table = start
        .checked_add(phnum * PHDR_SIZE)
        .and_then(|end| file.get(start..end))
        .ok_or(Error::OutsideFile)?;
    Ok(table.as_chunks::<PHDR_SIZE>().0.iter())
}

/// The checked loadable segments and interpreter of `file`.
/// Room for the loadable segments of an image.
type Segments = [Option<Segment>; 16];

fn segments(file: &[u8]) -> Result<(Segments, Option<&[u8]>), Error> {
    let mut found: Segments = [None; 16];
    let mut count = 0;
    let mut interpreter = None;
    for p in headers(file)? {
        let (kind, flags) = (u32_at(p, 0), u32_at(p, 4));
        let (offset, vaddr) = (u64_at(p, 8), u64_at(p, 16));
        let (filesz, memsz) = (u64_at(p, 32), u64_at(p, 40));
        let in_file = offset
            .checked_add(filesz)
            .is_some_and(|end| end <= file.len() as u64);
        if kind == PT_INTERP {
            if !in_file {
                return Err(Error::OutsideFile);
            }
            let name = &file[offset as usize..(offset + filesz) as usize];
            match name.split_last() {
                Some((0, name)) if name == INTERPRETER => interpreter = Some(name),
                _ => return Err(Error::Interpreter),
            }
        }
        if kind != PT_LOAD {
            continue;
        }
        if !in_file {
            return Err(Error::OutsideFile);
        }
        if flags & PF_W != 0 && flags & PF_X != 0 {
            return Err(Error::WriteExecute);
        }
        let misaligned = offset % PAGE_SIZE != vaddr % PAGE_SIZE;
        if misaligned || filesz > memsz || (flags & PF_W == 0 && memsz > filesz) {
            return Err(Error::Unmappable);
        }
        vaddr.checked_add(memsz).ok_or(Error::Unmappable)?;
        let segment = Segment {
            flags,
            offset,
            vaddr,
            filesz,
            memsz,
        };
        for other in found.iter().flatten() {
            let other: &Segment = other;
            if vaddr < other.vaddr + other.memsz && other.vaddr < vaddr + memsz {
                return Err(Error::Overlap);
            }
        }
        *found.get_mut(count).ok_or(Error::Unmappable)? = Some(segment);
        count += 1;
    }
    if count == 0 {
        return Err(Error::Unmappable);
    }
    Ok((found, interpreter))
}

/// Maps `len` bytes of `mem` from `offset` into `process` at `addr`,
/// replacing the reservation there.
fn place(
    process: Handle,
    perms: u32,
    addr: u64,
    mem: Handle,
    offset: u64,
    len: u64,
) -> Result<(), Error> {
    // SAFETY: the call takes no pointers.
    check(unsafe { sys::vm_unmap(process, addr, len) })?;
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    check(unsafe { sys::vm_map(process, perms | SPECIFIC, addr, mem, offset, len, &mut at) })
}

/// A new memory object of `len` bytes.
fn anonymous(len: u64) -> Result<Handle, Error> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    check(unsafe { sys::mem_create(len, 0, &mut mem) })?;
    Ok(mem)
}

fn close(handle: Handle) {
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(handle) };
}

/// Reserves `len` bytes of `process` at an address the kernel chooses.
fn reserve(process: Handle, len: u64) -> Result<u64, Error> {
    let mem = anonymous(len)?;
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process, 0, 0, mem, 0, len, &mut at) };
    close(mem);
    check(status)?;
    Ok(at)
}

/// Maps one segment of `image` into `process`, its addresses moved by
/// `base`.
fn map_segment(process: Handle, image: &Image, base: u64, s: &Segment) -> Result<(), Error> {
    let mut perms = PERM_READ;
    if s.flags & PF_X != 0 {
        perms |= PERM_EXECUTE;
    }
    let start = page_down(s.vaddr);
    let file_offset = page_down(s.offset);
    if s.flags & PF_W == 0 {
        let len = page_up(s.vaddr + s.filesz)? - start;
        return place(process, perms, base + start, image.shared, file_offset, len);
    }
    perms |= PERM_WRITE;
    let file_end = s.vaddr + s.filesz;
    let file_pages = page_up(file_end)? - start;
    let end = page_up(s.vaddr + s.memsz)?;
    if s.filesz > 0 {
        let mem = (image.private)(file_offset, file_pages)?;
        // The bytes after the segment's file part on its last page come
        // from the file but must read as zero.
        let tail = start + file_pages - file_end;
        let zeros = [0u8; PAGE_SIZE as usize];
        // SAFETY: the pointer and length lie inside `zeros`.
        let status = unsafe { sys::mem_write(mem, file_end - start, zeros.as_ptr(), tail) };
        let placed =
            check(status).and_then(|()| place(process, perms, base + start, mem, 0, file_pages));
        close(mem);
        placed?;
    }
    let zero_start = start + if s.filesz > 0 { file_pages } else { 0 };
    if end > zero_start {
        let mem = anonymous(end - zero_start)?;
        let placed = place(process, perms, base + zero_start, mem, 0, end - zero_start);
        close(mem);
        placed?;
    }
    Ok(())
}

/// Checks `image` and maps its segments into `process`, at an address
/// the kernel chooses.
pub fn load<'a>(process: Handle, image: &Image<'a>) -> Result<Loaded<'a>, Error> {
    let (segments, interpreter) = segments(image.file)?;
    let segments = || segments.iter().flatten();
    let low = segments().map(|s| page_down(s.vaddr)).min().unwrap_or(0);
    let mut high = 0;
    for s in segments() {
        high = high.max(page_up(s.vaddr + s.memsz)?);
    }
    let at = reserve(process, high - low)?;
    let base = at - low;
    for s in segments() {
        map_segment(process, image, base, s)?;
    }
    let file = image.file;
    let phoff = u64_at(file, 32);
    let mut phdr = None;
    for p in headers(file)? {
        if u32_at(p, 0) == PT_PHDR {
            phdr = Some(u64_at(p, 16));
        }
    }
    let phdr = phdr.or_else(|| {
        segments()
            .find(|s| (s.offset..s.offset + s.filesz).contains(&phoff))
            .map(|s| s.vaddr + (phoff - s.offset))
    });
    Ok(Loaded {
        base,
        entry: base + u64_at(file, 24),
        phdr: base + phdr.ok_or(Error::Unmappable)?,
        phnum: u32::from(u16_at(file, 56)),
        interpreter,
    })
}

/// Maps the main thread's stack of `STACK_SIZE` bytes into `process` at
/// an address the kernel chooses, with a reserved page below it that
/// cannot be accessed, and returns its top.
pub fn stack(process: Handle) -> Result<u64, Error> {
    let at = reserve(process, STACK_SIZE + PAGE_SIZE)?;
    let mem = anonymous(STACK_SIZE)?;
    let placed = place(
        process,
        PERM_READ | PERM_WRITE,
        at + PAGE_SIZE,
        mem,
        0,
        STACK_SIZE,
    );
    close(mem);
    placed?;
    Ok(at + PAGE_SIZE + STACK_SIZE)
}
