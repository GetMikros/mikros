//! Budgets: limits on the kernel memory and pages of groups of processes.

use super::{Header, Object, ObjectType, signals};
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The limit or threshold that is never reached.
pub const UNLIMITED: u64 = u64::MAX;

/// The topic of the budget information record.
pub const INFO_BUDGET: u32 = 6;
/// The size of the budget information record.
pub const INFO_BUDGET_SIZE: usize = 0x38;

/// Serializes every change to usage, limits and thresholds, so that a
/// charge sees the usage of each budget on its path as it is, without
/// parts of other charges that are about to be undone.
static ACCOUNTS: SpinLock<()> = SpinLock::new(());

/// One kind of usage. The fields change only under `ACCOUNTS`; they are
/// atomic so that they can be read without it.
struct Account {
    limit: AtomicU64,
    used: AtomicU64,
    threshold: AtomicU64,
}

impl Account {
    fn new(limit: u64) -> Account {
        Account {
            limit: AtomicU64::new(limit),
            used: AtomicU64::new(0),
            threshold: AtomicU64::new(UNLIMITED),
        }
    }

    fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    fn fits(&self, amount: u64) -> bool {
        let limit = self.limit.load(Ordering::Relaxed);
        self.used()
            .checked_add(amount)
            .is_some_and(|total| total <= limit)
    }

    fn over_threshold(&self) -> bool {
        self.used() >= self.threshold.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Kernel,
    Pages,
}

pub struct Budget {
    header: Header,
    parent: Option<Arc<Budget>>,
    kernel: Account,
    pages: Account,
    process_count: AtomicU32,
    /// The budget charged `BUDGET_COST` for this one.
    charged: Option<Arc<Budget>>,
}

/// The kernel memory charged for a budget created by a system call.
pub const BUDGET_COST: u64 = size_of::<Budget>() as u64;

impl Budget {
    /// The root budget, with no limits.
    pub fn root() -> Arc<Budget> {
        Arc::new(Budget::with(None, UNLIMITED, UNLIMITED))
    }

    /// A child budget of `parent` with the given limits and no usage. The
    /// child keeps its parent alive. When `charge` is given, `BUDGET_COST`
    /// is charged to it, and released when the child is dropped.
    pub fn child(
        parent: &Arc<Budget>,
        kernel_limit: u64,
        page_limit: u64,
        charge: Option<&Arc<Budget>>,
    ) -> Result<Arc<Budget>, Error> {
        if let Some(charge) = charge {
            charge.charge_kernel(BUDGET_COST)?;
        }
        let mut child = Budget::with(Some(parent.clone()), kernel_limit, page_limit);
        child.charged = charge.cloned();
        crate::fallible::try_arc(child)
    }

    fn with(parent: Option<Arc<Budget>>, kernel_limit: u64, page_limit: u64) -> Budget {
        Budget {
            charged: None,
            header: Header::new(ObjectType::Budget),
            parent,
            kernel: Account::new(kernel_limit),
            pages: Account::new(page_limit),
            process_count: AtomicU32::new(0),
        }
    }

    pub fn parent(&self) -> Option<&Arc<Budget>> {
        self.parent.as_ref()
    }

    fn account(&self, kind: Kind) -> &Account {
        match kind {
            Kind::Kernel => &self.kernel,
            Kind::Pages => &self.pages,
        }
    }

    /// This budget and its ancestors, nearest first.
    fn path(&self) -> impl Iterator<Item = &Budget> {
        core::iter::successors(Some(self), |b| b.parent.as_deref())
    }

    /// Sets or clears `OVER_THRESHOLD` to match the usage. Runs under
    /// `ACCOUNTS`.
    fn update_signal(&self) {
        if self.kernel.over_threshold() || self.pages.over_threshold() {
            self.header.update_signals(0, signals::OVER_THRESHOLD);
        } else {
            self.header.update_signals(signals::OVER_THRESHOLD, 0);
        }
    }

    fn charge(&self, kind: Kind, amount: u64) -> Result<(), Error> {
        let _lock = ACCOUNTS.lock();
        if !self.path().all(|b| b.account(kind).fits(amount)) {
            return Err(Error::NoResources);
        }
        for b in self.path() {
            b.account(kind).used.fetch_add(amount, Ordering::Relaxed);
            b.update_signal();
        }
        Ok(())
    }

    fn release(&self, kind: Kind, amount: u64) {
        let _lock = ACCOUNTS.lock();
        for b in self.path() {
            let used = &b.account(kind).used;
            let left = used.load(Ordering::Relaxed).checked_sub(amount);
            used.store(
                left.unwrap_or_else(|| panic!("released more than was charged to a budget")),
                Ordering::Relaxed,
            );
            b.update_signal();
        }
    }

    /// Charges `bytes` of kernel memory to this budget and its ancestors,
    /// or fails with no effect when that would exceed a limit.
    pub fn charge_kernel(&self, bytes: u64) -> Result<(), Error> {
        self.charge(Kind::Kernel, bytes)
    }

    /// Releases `bytes` of kernel memory charged by `charge_kernel`.
    pub fn release_kernel(&self, bytes: u64) {
        self.release(Kind::Kernel, bytes)
    }

    /// Charges `pages` pages to this budget and its ancestors, or fails
    /// with no effect when that would exceed a limit.
    pub fn charge_pages(&self, pages: u64) -> Result<(), Error> {
        self.charge(Kind::Pages, pages)
    }

    /// Releases `pages` pages charged by `charge_pages`.
    pub fn release_pages(&self, pages: u64) {
        self.release(Kind::Pages, pages)
    }

    pub fn set_limits(&self, kernel_limit: u64, page_limit: u64) {
        let _lock = ACCOUNTS.lock();
        self.kernel.limit.store(kernel_limit, Ordering::Relaxed);
        self.pages.limit.store(page_limit, Ordering::Relaxed);
    }

    pub fn set_thresholds(&self, kernel_threshold: u64, page_threshold: u64) {
        let _lock = ACCOUNTS.lock();
        self.kernel
            .threshold
            .store(kernel_threshold, Ordering::Relaxed);
        self.pages
            .threshold
            .store(page_threshold, Ordering::Relaxed);
        self.update_signal();
    }

    /// Counts a process that belongs to this budget.
    pub fn add_process(&self) {
        self.process_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Stops counting a process counted by `add_process`.
    pub fn remove_process(&self) {
        self.process_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// The budget information record.
    pub fn info(&self) -> [u8; INFO_BUDGET_SIZE] {
        let (kernel, pages) = {
            let _lock = ACCOUNTS.lock();
            let read = |a: &Account| {
                (
                    a.limit.load(Ordering::Relaxed),
                    a.used(),
                    a.threshold.load(Ordering::Relaxed),
                )
            };
            (read(&self.kernel), read(&self.pages))
        };
        let mut record = [0u8; INFO_BUDGET_SIZE];
        let mut put = |at: usize, v: u64| record[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put(0x00, kernel.0);
        put(0x08, kernel.1);
        put(0x10, pages.0);
        put(0x18, pages.1);
        put(0x28, kernel.2);
        put(0x30, pages.2);
        let count = self.process_count.load(Ordering::Relaxed);
        record[0x20..0x24].copy_from_slice(&count.to_le_bytes());
        record
    }
}

impl Object for Budget {
    fn header(&self) -> &Header {
        &self.header
    }
}

impl Drop for Budget {
    fn drop(&mut self) {
        if let Some(charged) = &self.charged {
            charged.release_kernel(BUDGET_COST);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(record: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(record[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn charges_the_whole_path_or_nothing() {
        let root = Budget::root();
        let mid = Budget::child(&root, 1000, UNLIMITED, None).unwrap();
        let leaf = Budget::child(&mid, UNLIMITED, 5, None).unwrap();
        let sibling = Budget::child(&mid, UNLIMITED, UNLIMITED, None).unwrap();
        leaf.charge_kernel(600).unwrap();
        assert_eq!(sibling.charge_kernel(401), Err(Error::NoResources));
        assert_eq!(mid.kernel.used(), 600);
        assert_eq!(root.kernel.used(), 600);
        sibling.charge_kernel(400).unwrap();
        assert_eq!(leaf.charge_pages(6), Err(Error::NoResources));
        leaf.charge_pages(5).unwrap();
        assert_eq!(root.pages.used(), 5);
        leaf.release_kernel(600);
        sibling.release_kernel(400);
        leaf.release_pages(5);
        assert_eq!((root.kernel.used(), root.pages.used()), (0, 0));
        assert_eq!(root.charge_kernel(u64::MAX), Ok(()));
        assert_eq!(root.charge_kernel(1), Err(Error::NoResources));
    }

    #[test]
    fn lets_limits_drop_below_usage() {
        let root = Budget::root();
        let b = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
        b.charge_pages(10).unwrap();
        b.set_limits(UNLIMITED, 4);
        assert_eq!(b.charge_pages(1), Err(Error::NoResources));
        b.release_pages(7);
        b.charge_pages(1).unwrap();
        assert_eq!(b.pages.used(), 4);
    }

    #[test]
    fn signals_usage_over_a_threshold() {
        let root = Budget::root();
        let b = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
        b.set_thresholds(100, UNLIMITED);
        root.set_thresholds(UNLIMITED, 2);
        let over = |b: &Budget| b.header.signals() & signals::OVER_THRESHOLD != 0;
        b.charge_kernel(99).unwrap();
        assert!(!over(&b));
        b.charge_kernel(1).unwrap();
        assert!(over(&b));
        b.charge_pages(2).unwrap();
        assert!(over(&root));
        b.release_pages(1);
        assert!(!over(&root));
        b.set_thresholds(UNLIMITED, UNLIMITED);
        assert!(!over(&b));
    }

    #[test]
    fn writes_the_information_record() {
        let root = Budget::root();
        let b = Budget::child(&root, 1 << 20, 64, None).unwrap();
        let child = Budget::child(&b, UNLIMITED, UNLIMITED, None).unwrap();
        child.charge_kernel(300).unwrap();
        child.charge_pages(3).unwrap();
        b.add_process();
        b.add_process();
        b.remove_process();
        b.set_thresholds(1 << 19, UNLIMITED);
        let r = b.info();
        assert_eq!(word(&r, 0x00), 1 << 20);
        assert_eq!(word(&r, 0x08), 300);
        assert_eq!(word(&r, 0x10), 64);
        assert_eq!(word(&r, 0x18), 3);
        assert_eq!(&r[0x20..0x28], &[1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(word(&r, 0x28), 1 << 19);
        assert_eq!(word(&r, 0x30), UNLIMITED);
    }

    #[test]
    fn children_keep_parents_alive() {
        let root = Budget::root();
        let weak = Arc::downgrade(&root);
        let child = Budget::child(&root, UNLIMITED, UNLIMITED, None).unwrap();
        drop(root);
        assert!(weak.upgrade().is_some());
        drop(child);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn charges_a_created_budget_to_the_creator() {
        let root = Budget::root();
        let creator = Budget::child(&root, BUDGET_COST, UNLIMITED, None).unwrap();
        let b = Budget::child(&root, UNLIMITED, UNLIMITED, Some(&creator)).unwrap();
        assert_eq!(word(&creator.info(), 0x08), BUDGET_COST);
        assert_eq!(word(&b.info(), 0x08), 0);
        assert_eq!(
            Budget::child(&b, 1, 1, Some(&creator)).err(),
            Some(Error::NoResources)
        );
        drop(b);
        assert_eq!(word(&root.info(), 0x08), 0);
    }
}
