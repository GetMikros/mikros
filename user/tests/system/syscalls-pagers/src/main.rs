//! The kernel's pagers, as a system test: paged objects that a pager
//! thread of the test answers, the pager's operations on them, frames
//! lent to them, writebacks and discards, operations on large objects,
//! paged buffers as the outputs of calls, the access flag, revoking
//! during a supply, and the faults on pages that cannot be supplied.

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
            ("paged_outputs", paged_outputs),
            ("access_flag_cleared_on_entry", access_flag_cleared_on_entry),
            (
                "seal_and_revoke_wait_for_supply",
                seal_and_revoke_wait_for_supply,
            ),
            (
                "faults_on_failed_revoked_and_cut_pages",
                faults_on_failed_revoked_and_cut_pages,
            ),
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

/// Outputs on absent pages of a paged object wait for the pager instead
/// of failing.
fn paged_outputs(s: &Own) -> Result {
    let before = REQUESTS.load(Ordering::Acquire);
    let (_pager, mem) = paged_object(s, 5, 0)?;
    let at = map(s.process, mem, 5 * PAGE_SIZE)?;
    let len = (at + PAGE_SIZE) as *mut u64;
    // SAFETY: the record and its size lie in the writable mapping.
    let status = unsafe { sys::object_get_info(mem, 1, at as *mut u8, 64, len) };
    check("reading information into absent pages", status)?;
    // SAFETY: the mapping is readable.
    let size = unsafe { len.read_volatile() };
    ensure("writing the size", size == 24)?;
    let ends = (at + 2 * PAGE_SIZE) as *mut Handle;
    // SAFETY: both handle outputs lie in the writable mapping.
    let status = unsafe { sys::channel_create(0, ends, ends.add(1)) };
    check("creating a channel into an absent page", status)?;
    // SAFETY: the mapping is readable.
    let (a, b) = unsafe { (ends.read_volatile(), ends.add(1).read_volatile()) };
    ensure("writing both handles", a != 0 && b != 0 && a != b)?;
    // SAFETY: the pointer and length describe the message.
    check("writing a message", unsafe {
        sys::channel_write(a, b"paged".as_ptr(), 5, core::ptr::null(), 0)
    })?;
    let (got, sizes) = (
        (at + 3 * PAGE_SIZE) as *mut u8,
        (at + 4 * PAGE_SIZE) as *mut u64,
    );
    // SAFETY: the bytes and both sizes lie in the writable mapping.
    let status =
        unsafe { sys::channel_read(b, got, 16, core::ptr::null_mut(), 0, sizes, sizes.add(1)) };
    check("reading a message into absent pages", status)?;
    // SAFETY: the mapping is readable.
    let (bytes, len, count) = unsafe {
        (
            (got as *const [u8; 5]).read_volatile(),
            sizes.read_volatile(),
            sizes.add(1).read_volatile(),
        )
    };
    ensure(
        "delivering the message",
        &bytes == b"paged" && len == 5 && count == 0,
    )?;
    ensure(
        "requesting each page",
        REQUESTS.load(Ordering::Acquire) == before + 5,
    )
}

/// Faults that wait for a pager, in the copy routine of a system call
/// and in user code that set RFLAGS.AC, leave AC clear in the kernel,
/// which the debug kernel checks whenever it switches threads.
fn access_flag_cleared_on_entry(s: &Own) -> Result {
    let (_pager, mem) = paged_object(s, 2, 0)?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    let copy = create_memory(PAGE_SIZE)?;
    // SAFETY: the source lies in the mapping; reading it waits for the
    // pager.
    check("writing from an absent page", unsafe {
        sys::mem_write(copy, 0, at as *const u8, 6)
    })?;
    ensure(
        "copying the supplied page",
        mem_read_at::<6>(copy, 0)? == *b"page 0",
    )?;
    let byte: u8;
    // SAFETY: the second page is mapped readable. With AC set, only the
    // aligned stack words and one byte are accessed, so alignment checks
    // cannot fault.
    unsafe {
        core::arch::asm!(
            "pushfq",
            "bts qword ptr [rsp], 18",
            "popfq",
            "mov {byte}, byte ptr [{addr}]",
            "pushfq",
            "btr qword ptr [rsp], 18",
            "popfq",
            addr = in(reg) at + PAGE_SIZE,
            byte = out(reg_byte) byte,
        );
    }
    ensure("reading with AC set", byte == b'p')
}

/// The pages of the source of `supply_beside`: more than the kernel
/// supplies in one piece, so that other threads run in between.
const SUPPLIED_PAGES: u64 = 4 * 512 + 1;

/// Set when the supply of `supply_beside` is about to begin, and the
/// first 8 bytes of the last page of its source as `sealer` or
/// `revoker` read them right after their call returned.
static SUPPLY_BEGUN: AtomicU64 = AtomicU64::new(0);

static SEEN_AFTER: AtomicU64 = AtomicU64::new(0);

/// Records the start of the last page of the supply's `source`.
fn see_last_page(source: Handle) {
    let seen = mem_read_at::<8>(source, (SUPPLIED_PAGES - 1) * PAGE_SIZE);
    SEEN_AFTER.store(seen.map_or(u64::MAX, u64::from_le_bytes), Ordering::Release);
}

/// Seals `source` with `WRITE` once the supply has begun, then records
/// its last page.
extern "sysv64" fn sealer(source: u64, _: u64) -> ! {
    while SUPPLY_BEGUN.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    if seal(source as Handle, 1 << 3) == 0 {
        see_last_page(source as Handle);
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Revokes `paged` of the pager in `PAGER` once the supply has begun,
/// then records the last page of `source`.
extern "sysv64" fn revoker(paged: u64, source: u64) -> ! {
    while SUPPLY_BEGUN.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    let pager = PAGER.load(Ordering::Acquire) as Handle;
    // SAFETY: the call takes no pointers.
    if unsafe { sys::pager_revoke(pager, paged as Handle) } == 0 {
        see_last_page(source as Handle);
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Supplies a new paged object from a source of `SUPPLIED_PAGES` pages
/// while another thread seals the source, when `sealing`, or revokes the
/// object, and checks that the supply did not change the source after
/// that call returned: it ended first, or it was refused.
fn supply_beside(s: &Own, sealing: bool) -> Result {
    let len = SUPPLIED_PAGES * PAGE_SIZE;
    let source = create_memory(len)?;
    mem_write_at(source, len - PAGE_SIZE, b"lastpage")?;
    let (mut pager, mut port, mut paged) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 13, len, 0, &mut paged);
        check("creating a paged object", status)?;
    }
    PAGER.store(u64::from(pager), Ordering::Release);
    SUPPLY_BEGUN.store(0, Ordering::Release);
    SEEN_AFTER.store(u64::MAX - 1, Ordering::Release);
    let helper = match sealing {
        true => spawn2(s, sealer, u64::from(source), 0)?,
        false => spawn2(s, revoker, u64::from(paged), u64::from(source))?,
    };
    SUPPLY_BEGUN.store(1, Ordering::Release);
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::pager_supply(pager, paged, 0, len, source, 0) };
    ensure(
        "supplying or refusing the whole call",
        status == 0 || status == ACCESS_DENIED,
    )?;
    ensure(
        "ending the helper",
        wait(helper, TERMINATED, in_time()).0 == 0,
    )?;
    let last = u64::from_le_bytes(mem_read_at::<8>(source, len - PAGE_SIZE)?);
    ensure(
        "leaving the source as it was when the call returned",
        SEEN_AFTER.load(Ordering::Acquire) == last,
    )?;
    for handle in [source, paged, port, pager] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(handle) })?;
    }
    Ok(())
}

/// A `WRITE` seal of a source and a revocation of the paged object wait
/// for a supply that is under way.
fn seal_and_revoke_wait_for_supply(s: &Own) -> Result {
    supply_beside(s, true)?;
    supply_beside(s, false)
}

/// Reads the byte at `addr`, which the test expects to fault, and exits.
extern "sysv64" fn fault_reader(addr: u64, _: u64) -> ! {
    // SAFETY: `addr` lies in a mapping; a fault is reported, and the test
    // kills the thread.
    unsafe { (addr as *const u8).read_volatile() };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Starts a thread reading `addr` and checks that it reports a
/// `PAGE_FAULT` at that address on `channel`; then kills the thread.
fn expect_page_fault(s: &Own, channel: Handle, addr: u64) -> Result {
    let thread = spawn2(s, fault_reader, addr, 0)?;
    let (r, reported) = report_bytes(channel)?;
    ensure(
        "reporting a page fault at the address",
        u32_at(&r, 0) == PAGE_FAULT
            && u64_at(&r, 0x18) == addr
            && u64_at(&r, 0x10) == koid(thread)?,
    )?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("killing the thread", sys::task_kill(thread))?;
        check("closing the reported thread", sys::handle_close(reported))?;
    }
    ensure(
        "ending the thread",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )
}

/// A user access faults with a `PAGE_FAULT` report on a page its pager
/// failed, on a page that was present when its object was revoked, and on
/// a page that a shrink cut off. The test gives this process an exception
/// channel, so that the faults are reported instead of ending it.
fn faults_on_failed_revoked_and_cut_pages(s: &Own) -> Result {
    const FAIL: u32 = 1;
    let mut channel = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating an exception channel", unsafe {
        sys::exception_channel_create(s.process, 0, &mut channel)
    })?;
    let paged_object = || -> Result<(Handle, Handle)> {
        let (mut pager, mut port, mut mem) = (0, 0, 0);
        // SAFETY: the pointers are valid for the calls.
        unsafe {
            check("creating a pager", sys::pager_create(0, &mut pager))?;
            check("creating a port", sys::port_create(0, &mut port))?;
            let status = sys::pager_create_memory(pager, port, 14, PAGE_SIZE, 0, &mut mem);
            check("creating a paged object", status)?;
        }
        Ok((pager, mem))
    };

    let (pager, failed) = paged_object()?;
    check(
        "failing the page",
        pager_op(pager, failed, FAIL, 0, PAGE_SIZE),
    )?;
    expect_page_fault(s, channel, map(s.process, failed, PAGE_SIZE)?)?;

    let (pager, revoked) = paged_object()?;
    // SAFETY: the call takes no pointers.
    check("supplying the page", unsafe {
        sys::pager_supply(pager, revoked, 0, PAGE_SIZE, create_memory(PAGE_SIZE)?, 0)
    })?;
    let at = map(s.process, revoked, PAGE_SIZE)?;
    // SAFETY: the page is mapped and present.
    unsafe { (at as *const u8).read_volatile() };
    // SAFETY: the call takes no pointers.
    check("revoking", unsafe { sys::pager_revoke(pager, revoked) })?;
    expect_page_fault(s, channel, at)?;

    let mut cut = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a resizable object", unsafe {
        sys::mem_create(2 * PAGE_SIZE, 1, &mut cut)
    })?;
    let at = map(s.process, cut, 2 * PAGE_SIZE)?;
    // SAFETY: both pages are mapped writable.
    unsafe { ((at + PAGE_SIZE) as *mut u8).write_volatile(1) };
    // SAFETY: the call takes no pointers.
    check("shrinking", unsafe { sys::mem_set_size(cut, PAGE_SIZE) })?;
    expect_page_fault(s, channel, at + PAGE_SIZE)?;
    // SAFETY: the call takes no pointers.
    check("closing the exception channel", unsafe {
        sys::handle_close(channel)
    })
}
