//! A pager thread that answers the page requests of a paged object, and
//! the dirty ranges of a paged object, for the tests of pagers in both
//! tiers.

use crate::{
    Context, PAGE_SIZE, Result, check, create_memory, mem_write_at, now, spawn2, take_packet,
    u32_at, u64_at,
};
use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::sys::{self, Handle};

/// The pager handle of the pager test, and how many page requests its
/// pager thread answered.
pub static PAGER: AtomicU64 = AtomicU64::new(0);
pub static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// Answers the page requests on `port` for `mem` until none comes for a
/// second, supplying pages that start with "page N".
pub extern "sysv64" fn pager_thread(port: u64, mem: u64) -> ! {
    let pager = PAGER.load(Ordering::Acquire) as Handle;
    loop {
        let (status, p) = take_packet(port as Handle, now() + 1_000_000_000);
        if status != 0 || u32_at(&p, 8) != 3 || u32_at(&p, 16) != 1 {
            break;
        }
        REQUESTS.fetch_add(1, Ordering::AcqRel);
        let offset = u64_at(&p, 24);
        let Ok(source) = create_memory(PAGE_SIZE) else {
            break;
        };
        let text = [
            b'p',
            b'a',
            b'g',
            b'e',
            b' ',
            // Tests check the label of the first pages only.
            b'0'.wrapping_add((offset / PAGE_SIZE) as u8),
        ];
        let _ = mem_write_at(source, 0, &text);
        // SAFETY: the call takes no pointers.
        unsafe { sys::pager_supply(pager, mem as Handle, offset, PAGE_SIZE, source, 0) };
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A new paged object of `pages` pages with `options`, answered by a
/// pager thread, and its pager.
pub fn paged_object(s: &impl Context, pages: u64, options: u32) -> Result<(Handle, Handle)> {
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 6, pages * PAGE_SIZE, options, &mut mem);
        check("creating a paged object", status)?;
    }
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, pager_thread, u64::from(port), u64::from(mem))?;
    Ok((pager, mem))
}

/// The dirty ranges of `mem` as byte offsets and lengths.
pub fn dirty(pager: Handle, mem: Handle, len: u64) -> Result<([u64; 8], u64)> {
    let mut out = [0u64; 8];
    let mut count = 0;
    // SAFETY: the pointers describe `out` and `count`.
    let status = unsafe {
        sys::pager_query_dirty(pager, mem, 0, len, out.as_mut_ptr().cast(), 4, &mut count)
    };
    check("querying dirty pages", status)?;
    Ok((out, count))
}
