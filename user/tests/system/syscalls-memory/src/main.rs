//! The kernel's memory objects, as a system test: creating, mapping,
//! sealing and cloning them, and reading one into itself.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::Managed;
use mikros_rt::sys;
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("memory_objects", memory_objects),
            ("shared_mappings", shared_mappings),
            ("seals", seals),
            ("clones", clones),
            ("mem_read_into_its_source", mem_read_into_its_source),
        ],
    )
}

/// A memory object mapped into this process shows the same bytes
/// through the mapping and through `mem_read`.
fn memory_objects(s: &Own) -> Result {
    let mem = create_memory(2 * PAGE_SIZE)?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    // SAFETY: the mapping is 8192 bytes long, readable and writable.
    unsafe { ((at + PAGE_SIZE + 8) as *mut u64).write_volatile(0x1234_5678_9ABC) };
    let mut back = [0u8; 8];
    // SAFETY: the pointer and length describe `back`.
    let status = unsafe { sys::mem_read(mem, PAGE_SIZE + 8, back.as_mut_ptr(), 8) };
    check("reading the object", status)?;
    ensure(
        "comparing the bytes",
        u64::from_le_bytes(back) == 0x1234_5678_9ABC,
    )?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, 2 * PAGE_SIZE)
    })?;
    // SAFETY: the call takes no pointers.
    check("closing the object", unsafe { sys::handle_close(mem) })
}

/// Two mappings of one object stay in step when a page read through one
/// as zeros gets a frame through the other, is decommitted, and is
/// written by the kernel.
fn shared_mappings(s: &Own) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let a = map(s.process, mem, PAGE_SIZE)? as *mut u64;
    let b = map(s.process, mem, PAGE_SIZE)? as *mut u64;
    // SAFETY: both mappings are a readable and writable page of `mem`.
    unsafe {
        ensure("reading zeros through the first", a.read_volatile() == 0)?;
        b.write_volatile(0x5eed);
        ensure(
            "seeing the write through the first",
            a.read_volatile() == 0x5eed,
        )?;
    }
    // SAFETY: the call takes no pointers.
    check("decommitting", unsafe { sys::mem_op(mem, 2, 0, PAGE_SIZE) })?;
    // SAFETY: as above.
    unsafe {
        ensure("reading zeros after decommit", a.read_volatile() == 0)?;
        ensure("reading zeros through both", b.read_volatile() == 0)?;
    }
    mem_write_at(mem, 0, &7u64.to_le_bytes())?;
    // SAFETY: as above.
    unsafe {
        ensure(
            "seeing the kernel write",
            a.read_volatile() == 7 && b.read_volatile() == 7,
        )?;
    }
    for at in [a, b] {
        // SAFETY: the call takes no pointers.
        check("unmapping", unsafe {
            sys::vm_unmap(s.process, at as u64, PAGE_SIZE)
        })?;
    }
    // SAFETY: the call takes no pointers.
    check("closing the object", unsafe { sys::handle_close(mem) })
}

/// Seals stop resizing, every way of writing, and further sealing.
fn seals(s: &Own) -> Result {
    let mut mem = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating", unsafe {
        sys::mem_create(2 * PAGE_SIZE, 1, &mut mem)
    })?;
    ensure("refusing other bits", seal(mem, 1 << 4) == -10)?;
    check("sealing the size", seal(mem, 2 | 4))?;
    ensure("refusing to shrink", set_size(mem, PAGE_SIZE) == -13)?;
    ensure("refusing to grow", set_size(mem, 3 * PAGE_SIZE) == -13)?;
    check("keeping the size", set_size(mem, 2 * PAGE_SIZE))?;

    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    ensure("refusing while mapped writable", seal(mem, 8) == -20)?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, 2 * PAGE_SIZE)
    })?;
    check("sealing writes", seal(mem, 8))?;
    let byte = [1u8];
    // SAFETY: the pointer and length describe `byte`.
    ensure(
        "refusing mem_write",
        unsafe { sys::mem_write(mem, 0, byte.as_ptr(), 1) } == -13,
    )?;
    // SAFETY: the call takes no pointers.
    ensure(
        "refusing decommit",
        unsafe { sys::mem_op(mem, 2, 0, PAGE_SIZE) } == -13,
    )?;
    let mut out = 0;
    // SAFETY: the pointer is valid for the call.
    let writable = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ | PERM_WRITE,
            0,
            mem,
            0,
            PAGE_SIZE,
            &mut out,
        )
    };
    ensure("refusing writable mappings", writable == -13)?;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::vm_map(s.process, PERM_READ, 0, mem, 0, PAGE_SIZE, &mut out) };
    check("mapping read-only", status)?;
    ensure(
        "refusing to make it writable",
        protect(s, PERM_READ | PERM_WRITE, out, PAGE_SIZE) == -13,
    )?;
    let record = info::<0x18>(mem, 4)?;
    ensure("reporting the seals", u32_at(&record, 0x10) == 2 | 4 | 8)?;
    check("sealing sealing", seal(mem, 1))?;
    ensure("refusing more seals", seal(mem, 0) == -13)
}

/// A clone starts with the source range's bytes and is separate from it
/// afterwards.
fn clones(_: &Own) -> Result {
    let source = create_memory(3 * PAGE_SIZE)?;
    mem_write_at(source, PAGE_SIZE - 3, b"across pages")?;
    let mut clone = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, 0, 2 * PAGE_SIZE, &mut clone) };
    check("cloning", status)?;
    ensure(
        "copying the range",
        mem_read_at::<12>(clone, PAGE_SIZE - 3)? == *b"across pages",
    )?;
    ensure(
        "reading zero elsewhere",
        mem_read_at::<8>(clone, 200)? == [0; 8],
    )?;
    mem_write_at(source, PAGE_SIZE - 3, b"AC")?;
    ensure(
        "keeping the clone",
        mem_read_at::<4>(clone, PAGE_SIZE - 3)? == *b"acro",
    )?;
    mem_write_at(clone, PAGE_SIZE - 3, b"x")?;
    ensure(
        "keeping the source",
        mem_read_at::<4>(source, PAGE_SIZE - 3)? == *b"ACro",
    )?;
    let record = info::<0x18>(clone, 4)?;
    ensure("sizing the clone", u64_at(&record, 0) == 2 * PAGE_SIZE)?;
    let mut unaligned = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, PAGE_SIZE - 5, PAGE_SIZE, &mut unaligned) };
    ensure("refusing an unaligned offset", status == -10)?;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, 0, PAGE_SIZE + 100, &mut unaligned) };
    ensure("refusing an unaligned length", status == -10)?;
    // A whole page whose bytes never repeat with a period of the kernel's
    // copy chunk, so every chunk of the clone is checked.
    let mut page = [0u8; PAGE_SIZE as usize];
    for (i, b) in page.iter_mut().enumerate() {
        *b = (i / 3) as u8 ^ (i >> 9) as u8;
    }
    mem_write_at(source, PAGE_SIZE, &page)?;
    let mut whole = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, PAGE_SIZE, PAGE_SIZE, &mut whole) };
    check("cloning a page", status)?;
    ensure(
        "copying every chunk",
        mem_read_at::<{ PAGE_SIZE as usize }>(whole, 0)? == page,
    )?;
    // SAFETY: the pointer is valid for the call.
    let bad = unsafe { sys::mem_clone(source, 2, 0, 1, &mut clone) };
    ensure("refusing other options", bad == -10)
}

/// `mem_read` into a buffer mapped from its own source range gives the
/// bytes the range held when the call began, whichever way the two are
/// shifted against each other.
fn mem_read_into_its_source(s: &Own) -> Result {
    let len = 2 * PAGE_SIZE;
    let pattern = |i: u64| (i % 251) as u8;
    for (from, to) in [(0, 8), (8, 0), (0, PAGE_SIZE - 4)] {
        let mem = create_memory(3 * PAGE_SIZE)?;
        let at = map(s.process, mem, 3 * PAGE_SIZE)?;
        for i in 0..len + 16 {
            // SAFETY: the mapping is writable and covers the byte.
            unsafe { *((at + i) as *mut u8) = pattern(i) };
        }
        // SAFETY: the buffer lies in the mapping.
        check("reading into the source", unsafe {
            sys::mem_read(mem, from, (at + to) as *mut u8, len)
        })?;
        let intact = (0..len).all(|i| {
            // SAFETY: as above.
            unsafe { *((at + to + i) as *const u8) == pattern(from + i) }
        });
        ensure("reading the bytes as they were", intact)?;
        // SAFETY: the call takes no pointers.
        check("unmapping", unsafe {
            sys::vm_unmap(s.process, at, 3 * PAGE_SIZE)
        })?;
        close(mem)?;
    }
    Ok(())
}
