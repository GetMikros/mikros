//! Writing prepared outputs to their retained backing in bounded steps.
//!
//! Outputs are prepared page by page before anything is written: each
//! destination page is resolved to backing the kernel can write through
//! its own address space, and that backing stays alive and in place until
//! the outputs are released. Writing them then cannot fail, and needs no
//! lock, so it can stop at reschedule checks.

use alloc::sync::Arc;
use alloc::vec::Vec;
use kernel_core::object::budget::Budget;
use kernel_core::object::memory::PAGE_SIZE;
use kernel_core::status::Error;

/// The most bytes copied between two checks.
pub const COPY_STEP: usize = 64 * 1024;
/// The most pages processed between two checks.
pub const ITEM_STEP: usize = 512;

/// Counts copied bytes and processed pages, and calls `check` before
/// either would exceed its step since the last call.
pub struct Work<F: FnMut()> {
    bytes: usize,
    items: usize,
    check: F,
}

impl<F: FnMut()> Work<F> {
    pub fn new(check: F) -> Work<F> {
        Work {
            bytes: 0,
            items: 0,
            check,
        }
    }

    fn checked(&mut self) {
        (self.check)();
        self.bytes = 0;
        self.items = 0;
    }

    /// Accounts for copying `n` bytes, at most `COPY_STEP`.
    pub fn copy(&mut self, n: usize) {
        debug_assert!(n <= COPY_STEP);
        if self.bytes + n > COPY_STEP {
            self.checked();
        }
        self.bytes += n;
    }

    /// Accounts for processing one page.
    pub fn item(&mut self) {
        if self.items == ITEM_STEP {
            self.checked();
        }
        self.items += 1;
    }
}

/// A destination page held writable until it is released or dropped.
pub trait Page: Sized {
    /// The start of the page's bytes in the kernel's address space.
    fn base(&self) -> *mut u8;

    /// Releases the page, accounting for the pages that releasing it
    /// processes.
    fn release<F: FnMut()>(self, work: &mut Work<F>) {
        work.item();
        drop(self);
    }
}

/// Prepared outputs: for each, its user address, its length and the
/// index of its first page among `pages`.
pub struct Outputs<P: Page> {
    ranges: Vec<(u64, u64, usize)>,
    pages: Vec<P>,
    charged: Option<(Arc<Budget>, u64)>,
}

/// The first page and the number of pages of `len` bytes from `addr`.
fn span(addr: u64, len: u64) -> (u64, usize) {
    if len == 0 {
        return (0, 0);
    }
    let first = addr & !(PAGE_SIZE - 1);
    let end = (addr + len).next_multiple_of(PAGE_SIZE);
    (first, ((end - first) / PAGE_SIZE) as usize)
}

impl<P: Page> Outputs<P> {
    /// Prepares the outputs `ranges`, whose addresses and lengths have been
    /// validated, by calling `page` for each page of each of them. The
    /// kernel memory recording the pages is charged to `budget`. On failure,
    /// the pages prepared so far are released.
    pub fn prepare<F: FnMut()>(
        ranges: &[(u64, u64)],
        budget: Option<&Arc<Budget>>,
        work: &mut Work<F>,
        mut page: impl FnMut(u64) -> Result<P, Error>,
    ) -> Result<Outputs<P>, Error> {
        let count: usize = ranges.iter().map(|&(addr, len)| span(addr, len).1).sum();
        let bytes = (count * size_of::<P>() + ranges.len() * size_of::<(u64, u64, usize)>()) as u64;
        if let Some(budget) = budget {
            budget.charge_kernel(bytes)?;
        }
        let mut outputs = Outputs {
            ranges: Vec::new(),
            pages: Vec::new(),
            charged: budget.map(|b| (b.clone(), bytes)),
        };
        if outputs.ranges.try_reserve_exact(ranges.len()).is_err()
            || outputs.pages.try_reserve_exact(count).is_err()
        {
            outputs.release(work);
            return Err(Error::NoMemory);
        }
        for &(addr, len) in ranges {
            outputs.ranges.push((addr, len, outputs.pages.len()));
            let (first, pages) = span(addr, len);
            for i in 0..pages as u64 {
                work.item();
                match page(first + i * PAGE_SIZE) {
                    Ok(p) => outputs.pages.push(p),
                    Err(e) => {
                        outputs.release(work);
                        return Err(e);
                    }
                }
            }
        }
        Ok(outputs)
    }

    /// Writes `bytes` at offset `at` of output `output`, which they must
    /// fit.
    pub fn write<F: FnMut()>(&self, output: usize, at: u64, bytes: &[u8], work: &mut Work<F>) {
        let (addr, len, first) = self.ranges[output];
        assert!(
            at.checked_add(bytes.len() as u64)
                .is_some_and(|end| end <= len)
        );
        let start = addr & !(PAGE_SIZE - 1);
        let mut done = 0;
        while done < bytes.len() {
            let target = addr + at + done as u64;
            let inside = (target % PAGE_SIZE) as usize;
            let n = (PAGE_SIZE as usize - inside).min(bytes.len() - done);
            let page = &self.pages[first + ((target - start) / PAGE_SIZE) as usize];
            work.copy(n);
            // SAFETY: the page stays writable while held, and `n` bytes
            // from `inside` lie in it. The source is a kernel buffer, which
            // never lies in a destination page.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes[done..].as_ptr(), page.base().add(inside), n);
            }
            done += n;
        }
    }

    /// Releases every page, and the charge for recording them.
    pub fn release<F: FnMut()>(mut self, work: &mut Work<F>) {
        while let Some(page) = self.pages.pop() {
            page.release(work);
        }
    }
}

impl<P: Page> Drop for Outputs<P> {
    fn drop(&mut self) {
        if let Some((budget, bytes)) = self.charged.take() {
            budget.release_kernel(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Fake {
        bytes: Box<[u8; PAGE_SIZE as usize]>,
        addr: u64,
        live: Rc<Cell<usize>>,
    }

    impl Page for Fake {
        fn base(&self) -> *mut u8 {
            self.bytes.as_ptr().cast_mut()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.live.set(self.live.get() - 1);
        }
    }

    fn fake(live: &Rc<Cell<usize>>, addr: u64) -> Fake {
        live.set(live.get() + 1);
        Fake {
            bytes: Box::new([0xa5; PAGE_SIZE as usize]),
            addr,
            live: live.clone(),
        }
    }

    fn used(budget: &Budget) -> u64 {
        u64::from_le_bytes(budget.info()[8..16].try_into().unwrap())
    }

    #[test]
    fn checks_before_exceeding_either_step() {
        let checks = Cell::new(0);
        let mut work = Work::new(|| checks.set(checks.get() + 1));
        for _ in 0..16 {
            work.copy(4096);
        }
        assert_eq!(checks.get(), 0);
        work.copy(1);
        assert_eq!(checks.get(), 1);
        work.copy(COPY_STEP - 1);
        assert_eq!(checks.get(), 1);
        for _ in 0..ITEM_STEP {
            work.item();
        }
        assert_eq!(checks.get(), 1);
        work.item();
        assert_eq!(checks.get(), 2);
    }

    #[test]
    fn prepares_every_page_of_every_output_in_order() {
        let live = Rc::new(Cell::new(0));
        let seen = RefCell::new(Vec::new());
        let mut work = Work::new(|| {});
        let ranges = [(0x1ffe, 4), (0x5000, 0), (0x8000, 0x2001)];
        let outputs = Outputs::prepare(&ranges, None, &mut work, |addr| {
            seen.borrow_mut().push(addr);
            Ok(fake(&live, addr))
        })
        .unwrap();
        assert_eq!(*seen.borrow(), [0x1000, 0x2000, 0x8000, 0x9000, 0xa000]);
        let addrs: Vec<u64> = outputs.pages.iter().map(|p| p.addr).collect();
        assert_eq!(addrs, *seen.borrow());
        outputs.release(&mut work);
        assert_eq!(live.get(), 0);
    }

    #[test]
    fn failed_preparation_releases_pages_and_charge() {
        let live = Rc::new(Cell::new(0));
        let root = Budget::root();
        let mut work = Work::new(|| {});
        let result = Outputs::prepare(&[(0x1000, 0x3000)], Some(&root), &mut work, |addr| {
            if addr == 0x3000 {
                Err(Error::InvalidArgs)
            } else {
                Ok(fake(&live, addr))
            }
        });
        assert_eq!(result.err(), Some(Error::InvalidArgs));
        assert_eq!((live.get(), used(&root)), (0, 0));
    }

    #[test]
    fn charges_the_record_until_dropped() {
        let live = Rc::new(Cell::new(0));
        let root = Budget::root();
        let small = Budget::child(&root, 8, u64::MAX, None).unwrap();
        let mut work = Work::new(|| {});
        let refused = Outputs::prepare(&[(0x1000, 1)], Some(&small), &mut work, |a| {
            Ok(fake(&live, a))
        });
        assert_eq!(refused.err(), Some(Error::NoResources));
        let outputs = Outputs::prepare(&[(0x1000, 1)], Some(&root), &mut work, |a| {
            Ok(fake(&live, a))
        })
        .unwrap();
        assert!(used(&root) > 0);
        drop(outputs);
        assert_eq!((live.get(), used(&root)), (0, 0));
    }

    #[test]
    fn writes_across_pages_and_checks_per_step() {
        let live = Rc::new(Cell::new(0));
        let checks = Cell::new(0);
        let mut work = Work::new(|| checks.set(checks.get() + 1));
        let len = COPY_STEP as u64 + 300;
        let ranges = [(0x10_0ff0, len), (0x20_0000, 8)];
        let outputs = Outputs::prepare(&ranges, None, &mut work, |a| Ok(fake(&live, a))).unwrap();
        let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
        outputs.write(0, 0, &payload, &mut work);
        outputs.write(1, 2, &[7, 8], &mut work);
        assert_eq!(checks.get(), 1);
        let mut read = Vec::new();
        for (i, page) in outputs.pages[..18].iter().enumerate() {
            let from = if i == 0 { 0xff0 } else { 0 };
            read.extend_from_slice(&page.bytes[from..]);
        }
        assert_eq!(&read[..payload.len()], payload.as_slice());
        assert!(read[payload.len()..].iter().all(|&b| b == 0xa5));
        assert_eq!(&outputs.pages[0].bytes[..0xff0], &[0xa5; 0xff0][..]);
        assert_eq!(
            &outputs.pages[18].bytes[..6],
            &[0xa5, 0xa5, 7, 8, 0xa5, 0xa5]
        );
    }

    #[test]
    #[should_panic]
    fn refuses_writes_past_an_output() {
        let live = Rc::new(Cell::new(0));
        let mut work = Work::new(|| {});
        let outputs =
            Outputs::prepare(&[(0x1000, 4)], None, &mut work, |a| Ok(fake(&live, a))).unwrap();
        outputs.write(0, 2, &[1, 2, 3], &mut work);
    }
}
