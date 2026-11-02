//! Loading static ELF executables into new processes.

use crate::{PAGE_SIZE, PERM_EXECUTE, PERM_READ, PERM_WRITE, Result, SPECIFIC, check, ensure};
use mikros_rt::sys::{self, Handle};

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
/// The size of the stack of a loaded program's first thread.
const STACK_SIZE: u64 = 64 * 1024;

fn u16_at(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().expect("8 bytes"))
}

/// Maps a new memory object of `len` bytes into `process`, at `addr`
/// when it is not zero, after `fill` wrote it, and returns its address.
fn map(
    process: Handle,
    perms: u32,
    addr: u64,
    len: u64,
    fill: impl FnOnce(Handle) -> Result,
) -> Result<u64> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a segment", unsafe {
        sys::mem_create(len, 0, &mut mem)
    })?;
    let filled = fill(mem);
    let options = if addr == 0 { perms } else { perms | SPECIFIC };
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process, options, addr, mem, 0, len, &mut at) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::handle_close(mem) };
    filled?;
    check("mapping a segment", status)?;
    Ok(at)
}

/// A loaded program: its entry point and the top of its stack.
pub struct Loaded {
    pub entry: u64,
    pub stack: u64,
}

/// Maps the segments of the executable `file` and a stack into
/// `process`.
pub fn load(process: Handle, file: &[u8]) -> Result<Loaded> {
    ensure(
        "checking the ELF header",
        file.len() >= 64 && file[..4] == *b"\x7fELF",
    )?;
    let (phoff, phentsize, phnum) = (
        u64_at(file, 32) as usize,
        u16_at(file, 54),
        u16_at(file, 56),
    );
    for i in 0..usize::from(phnum) {
        let at = phoff + i * usize::from(phentsize);
        let p = file.get(at..at + 56);
        ensure("reading a program header", p.is_some())?;
        let p = p.unwrap_or_default();
        if u32_at(p, 0) != PT_LOAD {
            continue;
        }
        let (flags, offset, vaddr) = (u32_at(p, 4), u64_at(p, 8) as usize, u64_at(p, 16));
        let (filesz, memsz) = (u64_at(p, 32) as usize, u64_at(p, 40));
        let data = file.get(offset..offset + filesz);
        ensure("reading a segment", data.is_some())?;
        let data = data.unwrap_or_default();
        let start = vaddr & !(PAGE_SIZE - 1);
        let end = (vaddr + memsz).next_multiple_of(PAGE_SIZE);
        let mut perms = PERM_READ;
        if flags & PF_W != 0 {
            perms |= PERM_WRITE;
        }
        if flags & PF_X != 0 {
            perms |= PERM_EXECUTE;
        }
        map(process, perms, start, end - start, |mem| {
            // SAFETY: the pointer and length describe `data`.
            let status =
                unsafe { sys::mem_write(mem, vaddr - start, data.as_ptr(), data.len() as u64) };
            check("writing a segment", status)
        })?;
    }
    let stack = map(process, PERM_READ | PERM_WRITE, 0, STACK_SIZE, |_| Ok(()))?;
    Ok(Loaded {
        entry: u64_at(file, 24),
        stack: stack + STACK_SIZE,
    })
}
