//! The kernel's pagers, as a system test: paged objects that a pager
//! thread of the test answers, the pager's operations on them, frames
//! lent to them, writebacks and discards, and operations on large
//! objects.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("pagers", pagers),
            ("pager_operations", pager_operations),
            ("lending_frames", lending_frames),
            ("writebacks_and_discards", writebacks_and_discards),
            ("large_pager_operations", large_pager_operations),
        ],
    )
}

/// A paged object requests each absent page once, from mappings and
/// system calls, and gets the frames its pager supplies.
fn pagers(s: &Own) -> Result {
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 5, 2 * PAGE_SIZE, 0, &mut mem);
        check("creating a paged object", status)?;
    }
    ensure("reporting the kind", u32_at(&info::<0x18>(mem, 4)?, 8) == 5)?;
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, pager_thread, u64::from(port), u64::from(mem))?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    // SAFETY: the mapping covers both pages; reading them waits for the
    // pager.
    let (first, second) = unsafe {
        (
            (at as *const [u8; 6]).read_volatile(),
            ((at + PAGE_SIZE) as *const [u8; 6]).read_volatile(),
        )
    };
    ensure(
        "supplying through faults",
        first == *b"page 0" && second == *b"page 1",
    )?;
    ensure(
        "reading through mem_read",
        mem_read_at::<6>(mem, PAGE_SIZE)? == *b"page 1",
    )?;
    // SAFETY: the mapping is writable.
    unsafe { (at as *mut u8).write_volatile(b'P') };
    ensure("keeping writes", mem_read_at::<6>(mem, 0)? == *b"Page 0")?;
    ensure(
        "requesting each page once",
        REQUESTS.load(Ordering::Acquire) == 2,
    )?;

    let source = create_memory(PAGE_SIZE)?;
    mem_write_at(source, 0, b"left")?;
    let (mut other, plain) = (0, create_memory(PAGE_SIZE)?);
    // SAFETY: the pointer is valid for the call.
    check("creating another pager", unsafe {
        sys::pager_create(0, &mut other)
    })?;
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "needing its pager",
            sys::pager_supply(other, mem, 0, PAGE_SIZE, source, 0) == -13,
        )?;
        ensure(
            "needing a paged object",
            sys::pager_supply(pager, plain, 0, PAGE_SIZE, source, 0) == -2,
        )?;
        check(
            "supplying present pages",
            sys::pager_supply(pager, mem, 0, PAGE_SIZE, source, 0),
        )?;
    }
    ensure(
        "leaving present pages alone",
        mem_read_at::<6>(mem, 0)? == *b"Page 0",
    )?;
    ensure(
        "decommitting the source",
        mem_read_at::<4>(source, 0)? == [0; 4],
    )?;
    check("sealing a source", seal(plain, 8))?;
    // SAFETY: the call takes no pointers.
    let sealed = unsafe { sys::pager_supply(pager, mem, 0, PAGE_SIZE, plain, 0) };
    ensure("refusing a sealed source", sealed == -13)
}

/// Dirty tracking, writeback, eviction, failing, a pager's end and
/// revocation of paged objects.
fn pager_operations(s: &Own) -> Result {
    let size = 3 * PAGE_SIZE;
    let (pager, mem) = paged_object(s, 3, 2)?;
    ensure(
        "reporting dirty tracking",
        u32_at(&info::<0x18>(mem, 4)?, 0x0C) == 2,
    )?;
    for page in 0..3 {
        mem_read_at::<1>(mem, page * PAGE_SIZE)?;
    }
    ensure("supplying clean pages", dirty(pager, mem, size)?.1 == 0)?;
    mem_write_at(mem, PAGE_SIZE, b"w")?;
    let at = map(s.process, mem, size)?;
    // SAFETY: the mapping is writable.
    unsafe { ((at + 2 * PAGE_SIZE) as *mut u8).write_volatile(b'm') };
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "tracking writes",
        count == 1 && ranges[..2] == [PAGE_SIZE, 2 * PAGE_SIZE],
    )?;

    check("beginning writeback", pager_op(pager, mem, 3, 0, size))?;
    ensure(
        "refusing an overlapping writeback",
        pager_op(pager, mem, 3, 0, size) == BAD_STATE,
    )?;
    ensure(
        "naming the range",
        pager_op(pager, mem, 4, 0, PAGE_SIZE) == BAD_STATE,
    )?;
    // SAFETY: the mapping is writable.
    unsafe { ((at + PAGE_SIZE) as *mut u8).write_volatile(b'n') };
    check("ending writeback", pager_op(pager, mem, 4, 0, size))?;
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "keeping pages written meanwhile",
        count == 1 && ranges[..2] == [PAGE_SIZE, PAGE_SIZE],
    )?;
    check("beginning again", pager_op(pager, mem, 3, 0, size))?;
    check("aborting", pager_op(pager, mem, 5, 0, size))?;
    ensure("staying dirty", dirty(pager, mem, size)?.1 == 1)?;

    let before = REQUESTS.load(Ordering::Acquire);
    check("evicting", pager_op(pager, mem, 2, 0, size))?;
    ensure(
        "keeping dirty pages",
        mem_read_at::<1>(mem, PAGE_SIZE)? == *b"n",
    )?;
    ensure(
        "requesting evicted pages again",
        mem_read_at::<6>(mem, 0)? == *b"page 0",
    )?;
    ensure(
        "counting the request",
        REQUESTS.load(Ordering::Acquire) == before + 1,
    )?;
    ensure(
        "refusing other ops",
        pager_op(pager, mem, 7, 0, size) == -10,
    )?;

    // SAFETY: the call takes no pointers.
    check("revoking", unsafe { sys::pager_revoke(pager, mem) })?;
    // SAFETY: the call takes no pointers.
    check("revoking again", unsafe { sys::pager_revoke(pager, mem) })?;
    let mut byte = [0u8; 1];
    // SAFETY: the pointers describe `byte`.
    unsafe {
        ensure(
            "refusing reads",
            sys::mem_read(mem, 0, byte.as_mut_ptr(), 1) == -13,
        )?;
        ensure(
            "refusing writes",
            sys::mem_write(mem, 0, byte.as_ptr(), 1) == -13,
        )?;
        let mut out = 0;
        let status = sys::vm_map(s.process, PERM_READ, 0, mem, 0, PAGE_SIZE, &mut out);
        ensure("refusing mappings", status == -13)?;
    }

    let (pager, mem) = paged_object(s, 2, 0)?;
    check("failing a page", pager_op(pager, mem, 1, 0, PAGE_SIZE))?;
    // SAFETY: the pointers describe `byte`.
    ensure(
        "failing reads",
        unsafe { sys::mem_read(mem, 0, byte.as_mut_ptr(), 1) } == IO,
    )?;
    ensure(
        "needing dirty tracking",
        unsafe { sys::pager_query_dirty(pager, mem, 0, PAGE_SIZE, byte.as_mut_ptr(), 0, &mut 0) }
            == -2,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the pager", unsafe { sys::handle_close(pager) })?;
    // SAFETY: the pointers describe `byte`.
    ensure(
        "failing without a pager",
        unsafe { sys::mem_read(mem, PAGE_SIZE, byte.as_mut_ptr(), 1) } == IO,
    )
}

/// `pager_lend` makes absent pages of a paged object use the frames of an
/// anonymous object, which keeps them: writes through either show in
/// both, lent pages are clean, a clone copies them, the frames stay while
/// lent even when the source goes, eviction ends the lend so that the
/// page is requested again, and a sealed source or a revoked object is
/// refused.
fn lending_frames(s: &Own) -> Result {
    let (pager, mem) = paged_object(s, 2, 2)?;
    let source = create_memory(2 * PAGE_SIZE)?;
    mem_write_at(source, 0, b"lent 0")?;
    mem_write_at(source, PAGE_SIZE, b"lent 1")?;
    let before = REQUESTS.load(Ordering::Acquire);
    // SAFETY: the call takes no pointers.
    check("lending", unsafe {
        sys::pager_lend(pager, mem, 0, 2 * PAGE_SIZE, source, 0)
    })?;
    ensure(
        "reading the source's frames",
        mem_read_at::<6>(mem, 0)? == *b"lent 0" && mem_read_at::<6>(mem, PAGE_SIZE)? == *b"lent 1",
    )?;
    ensure(
        "requesting no page",
        REQUESTS.load(Ordering::Acquire) == before,
    )?;
    ensure(
        "lending clean pages",
        dirty(pager, mem, 2 * PAGE_SIZE)?.1 == 0,
    )?;
    mem_write_at(mem, 0, b"L")?;
    ensure(
        "showing writes to the paged object in the source",
        mem_read_at::<6>(source, 0)? == *b"Lent 0",
    )?;
    let at = map(s.process, source, PAGE_SIZE)?;
    // SAFETY: the mapping is writable.
    unsafe { ((at + 1) as *mut u8).write_volatile(b'E') };
    ensure(
        "showing writes to the source in the paged object",
        mem_read_at::<6>(mem, 0)? == *b"LEnt 0",
    )?;
    let (ranges, count) = dirty(pager, mem, 2 * PAGE_SIZE)?;
    ensure(
        "tracking writes to the paged object",
        count == 1 && ranges[..2] == [0, PAGE_SIZE],
    )?;

    let mut clone = 0;
    // SAFETY: the out pointer is valid for the call.
    check("cloning", unsafe {
        sys::mem_clone(mem, 0, 0, 2 * PAGE_SIZE, &mut clone)
    })?;
    // SAFETY: the mapping is writable.
    unsafe { (at as *mut u8).write_volatile(b'X') };
    ensure(
        "copying lent pages into the clone",
        mem_read_at::<6>(clone, 0)? == *b"LEnt 0",
    )?;
    // SAFETY: the calls take no pointers.
    unsafe {
        sys::vm_unmap(s.process, at, PAGE_SIZE);
        check("closing the source", sys::handle_close(source))?;
    }
    ensure(
        "keeping lent frames once the source is gone",
        mem_read_at::<6>(mem, 0)? == *b"XEnt 0" && mem_read_at::<6>(mem, PAGE_SIZE)? == *b"lent 1",
    )?;
    check("evicting", pager_op(pager, mem, 2, PAGE_SIZE, PAGE_SIZE))?;
    ensure(
        "requesting a page whose lend ended",
        mem_read_at::<6>(mem, PAGE_SIZE)? == *b"page 1",
    )?;

    let sealed = create_memory(PAGE_SIZE)?;
    check("sealing a source", seal(sealed, 8))?;
    check("discarding", pager_op(pager, mem, 6, 0, 2 * PAGE_SIZE))?;
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "refusing a sealed source",
            sys::pager_lend(pager, mem, 0, PAGE_SIZE, sealed, 0) == -13,
        )?;
        check("revoking", sys::pager_revoke(pager, mem))?;
        ensure(
            "refusing a revoked object",
            sys::pager_lend(pager, mem, 0, PAGE_SIZE, create_memory(PAGE_SIZE)?, 0) == -13,
        )
    }
}

/// Supplying, writing back and evicting a paged object of more pages
/// than the kernel handles in one piece reaches every page.
fn large_pager_operations(s: &Own) -> Result {
    let pages = 1300;
    let size = pages * PAGE_SIZE;
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 10, size, 2, &mut mem);
        check("creating a paged object", status)?;
    }
    // Every page of the source holds its number.
    let source = create_memory(size)?;
    let at = map(s.process, source, size)?;
    for page in 0..pages {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE) as *mut u64).write_volatile(page) };
    }
    // SAFETY: the call takes no pointers.
    check("unmapping the source", unsafe {
        sys::vm_unmap(s.process, at, size)
    })?;
    // SAFETY: the call takes no pointers.
    check("supplying", unsafe {
        sys::pager_supply(pager, mem, 0, size, source, 0)
    })?;
    let at = map(s.process, mem, size)?;
    for page in 0..pages {
        let page_at = (at + page * PAGE_SIZE) as *const u64;
        // SAFETY: the mapping is readable.
        ensure(
            "supplying every page",
            unsafe { page_at.read_volatile() } == page,
        )?;
        let left = u64::from_le_bytes(mem_read_at::<8>(source, page * PAGE_SIZE)?);
        ensure("taking every page from the source", left == 0)?;
    }
    ensure("supplying clean pages", dirty(pager, mem, size)?.1 == 0)?;
    for page in 0..pages {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE + 8) as *mut u8).write_volatile(1) };
    }
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "tracking every write",
        count == 1 && ranges[..2] == [0, size],
    )?;

    check("beginning writeback", pager_op(pager, mem, 3, 0, size))?;
    let written = [0, 700, pages - 1];
    for page in written {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE + 8) as *mut u8).write_volatile(2) };
    }
    check("ending writeback", pager_op(pager, mem, 4, 0, size))?;
    let (ranges, count) = dirty(pager, mem, size)?;
    let want = written.map(|p| [p * PAGE_SIZE, PAGE_SIZE]);
    ensure(
        "keeping only pages written meanwhile",
        count == 3 && ranges[..6] == *want.as_flattened(),
    )?;

    // Every clean page is gone after the eviction, so reading it asks
    // the pager for it; the dirty pages stay.
    check("evicting", pager_op(pager, mem, 2, 0, size))?;
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, pager_thread, u64::from(port), u64::from(mem))?;
    let before = REQUESTS.load(Ordering::Acquire);
    for page in written {
        let page_at = (at + page * PAGE_SIZE) as *const u64;
        // SAFETY: the mapping is readable.
        ensure(
            "keeping dirty pages",
            unsafe { page_at.read_volatile() } == page,
        )?;
    }
    for page in 0..pages {
        mem_read_at::<1>(mem, page * PAGE_SIZE)?;
    }
    ensure(
        "evicting every clean page",
        REQUESTS.load(Ordering::Acquire) == before + pages - 3,
    )?;
    // SAFETY: the calls take no pointers. Closing the port ends the
    // pager thread.
    unsafe {
        check("unmapping", sys::vm_unmap(s.process, at, size))?;
        for handle in [mem, source, port, pager] {
            check("closing", sys::handle_close(handle))?;
        }
    }
    Ok(())
}

/// Whether the write of a writer thread of `writebacks_and_discards` is
/// done, and the status of a system call that wrote.
static STABLE_WRITTEN: AtomicU64 = AtomicU64::new(0);

static STABLE_STATUS: AtomicI64 = AtomicI64::new(0);

/// Writes `value` to the byte at `addr`, through a mapping, and records
/// that it did.
extern "sysv64" fn stable_writer(addr: u64, value: u64) -> ! {
    // SAFETY: the address lies in a writable mapping.
    unsafe { (addr as *mut u8).write_volatile(value as u8) };
    STABLE_WRITTEN.store(1, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Writes a byte at `offset` of `mem` with `mem_write`, and records the
/// call's status.
extern "sysv64" fn stable_mem_writer(mem: u64, offset: u64) -> ! {
    let byte = b"y";
    // SAFETY: the pointer describes `byte`.
    let status = unsafe { sys::mem_write(mem as Handle, offset, byte.as_ptr(), 1) };
    STABLE_STATUS.store(status, Ordering::Release);
    STABLE_WRITTEN.store(1, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Waits until the writer thread of `writebacks_and_discards` is done.
fn stable_write_done() -> Result {
    let deadline = in_time();
    while STABLE_WRITTEN.load(Ordering::Acquire) == 0 {
        ensure("letting the write go on", now() < deadline)?;
        sleep_ms(1);
    }
    Ok(())
}

/// Several writebacks of one paged object are outstanding at once, but
/// not over the same pages, and each ends by its own range; `DISCARD`
/// removes dirty pages too, which are requested again, but not a page
/// that a writeback records; and with `STABLE`, which needs dirty
/// tracking, a write to a page being written back waits until the
/// writeback ends, and fails once the pager goes.
fn writebacks_and_discards(s: &Own) -> Result {
    let size = 4 * PAGE_SIZE;
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 6, size, 4, &mut mem);
        ensure("needing dirty tracking for STABLE", status == -10)?;
        sys::handle_close(port);
        sys::handle_close(pager);
    }
    let (pager, mem) = paged_object(s, 4, 2 | 4)?;
    ensure(
        "reporting STABLE",
        u32_at(&info::<0x18>(mem, 4)?, 0x0C) == 6,
    )?;
    let at = map(s.process, mem, size)?;
    for page in 0..4 {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE) as *mut u8).write_volatile(b'a' + page as u8) };
    }

    check(
        "beginning a writeback of page 0",
        pager_op(pager, mem, 3, 0, PAGE_SIZE),
    )?;
    check(
        "beginning a writeback of page 2",
        pager_op(pager, mem, 3, 2 * PAGE_SIZE, PAGE_SIZE),
    )?;
    ensure(
        "refusing an overlapping writeback",
        pager_op(pager, mem, 3, 0, 2 * PAGE_SIZE) == BAD_STATE,
    )?;
    ensure(
        "refusing to discard pages being written back",
        pager_op(pager, mem, 6, 0, size) == BAD_STATE,
    )?;
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure("keeping them", count == 1 && ranges[..2] == [0, size])?;

    STABLE_WRITTEN.store(0, Ordering::Release);
    spawn2(s, stable_writer, at, u64::from(b'z'))?;
    sleep_ms(50);
    ensure(
        "making the write wait",
        STABLE_WRITTEN.load(Ordering::Acquire) == 0,
    )?;
    check(
        "ending the writeback of page 0",
        pager_op(pager, mem, 4, 0, PAGE_SIZE),
    )?;
    stable_write_done()?;
    ensure("writing then", mem_read_at::<1>(mem, 0)? == *b"z")?;
    check(
        "ending the writeback of page 2",
        pager_op(pager, mem, 4, 2 * PAGE_SIZE, PAGE_SIZE),
    )?;
    // Page 0 was written again, and pages 1 and 3 were not written back.
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "cleaning only the pages written back",
        count == 2 && ranges[..4] == [0, 2 * PAGE_SIZE, 3 * PAGE_SIZE, PAGE_SIZE],
    )?;

    let before = REQUESTS.load(Ordering::Acquire);
    check("discarding", pager_op(pager, mem, 6, 0, size))?;
    ensure(
        "requesting discarded pages again",
        mem_read_at::<6>(mem, PAGE_SIZE)? == *b"page 1",
    )?;
    ensure(
        "counting the request",
        REQUESTS.load(Ordering::Acquire) == before + 1,
    )?;
    ensure("forgetting their writes", dirty(pager, mem, size)?.1 == 0)?;

    // A write that waits fails once the pager goes.
    mem_write_at(mem, PAGE_SIZE, b"x")?;
    check(
        "beginning a writeback of page 1",
        pager_op(pager, mem, 3, PAGE_SIZE, PAGE_SIZE),
    )?;
    STABLE_WRITTEN.store(0, Ordering::Release);
    spawn2(s, stable_mem_writer, u64::from(mem), PAGE_SIZE)?;
    sleep_ms(50);
    ensure(
        "making the system call wait",
        STABLE_WRITTEN.load(Ordering::Acquire) == 0,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the pager", unsafe { sys::handle_close(pager) })?;
    stable_write_done()?;
    ensure(
        "failing the write",
        STABLE_STATUS.load(Ordering::Acquire) == IO,
    )
}
