//! Frames that anonymous memory objects lend to paged ones with
//! `pager_lend`: how many pages use each, so that none is freed or given
//! to another object while one does.

use crate::nodes::{self, Map};
use crate::object::budget::Budget;
use crate::object::memory::PAGE_ENTRY_COST;
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// A lent frame.
struct Lend {
    /// The pages of paged objects that use the frame.
    count: u64,
    /// Once the object that lent the frame has let go of it, the budget
    /// that its charge goes back to when the last lend ends, `None` for a
    /// frame charged to nothing.
    orphan: Option<Option<Arc<Budget>>>,
}

/// The lent frames, by physical address.
static LENDS: SpinLock<Map<u64, Lend>> =
    SpinLock::new(crate::lock_order::FRAME_LENDS, nodes::map());

/// How many frames `LENDS` holds, so that freeing frames while none is
/// lent need not look. A frame is lent with the lending object's pages
/// locked, which they are again before that object can free the frame.
static LENT: AtomicU64 = AtomicU64::new(0);

/// Records one more page that uses the frame `phys`. Fails with
/// `NoMemory`, changing nothing, without room to record it.
pub fn lend(phys: u64) -> Result<(), Error> {
    let mut lends = LENDS.lock();
    if let Some(lend) = lends.get_mut(&phys) {
        lend.count += 1;
        return Ok(());
    }
    let _claim = nodes::claim_insert()?;
    lends.insert(
        phys,
        Lend {
            count: 1,
            orphan: None,
        },
    );
    LENT.fetch_add(1, Ordering::Release);
    Ok(())
}

/// Whether a page uses the frame `phys` as a lent frame.
pub fn is_lent(phys: u64) -> bool {
    LENT.load(Ordering::Acquire) != 0 && LENDS.lock().contains_key(&phys)
}

/// Ends one lend of the frame `phys`. When it was the last and the object
/// that lent the frame has let go of it, the frame is freed and its
/// charge released.
pub fn end(phys: u64) {
    let orphan = {
        let mut lends = LENDS.lock();
        let Some(lend) = lends.get_mut(&phys) else {
            return;
        };
        lend.count -= 1;
        if lend.count > 0 {
            return;
        }
        LENT.fetch_sub(1, Ordering::Release);
        lends.remove(&phys).and_then(|lend| lend.orphan)
    };
    let Some(budget) = orphan else {
        return;
    };
    if let Some(allocator) = crate::frames::FRAMES.lock().as_mut() {
        allocator.free(phys, 1);
    }
    if let Some(budget) = budget {
        budget.release_pages(1);
        budget.release_kernel(PAGE_ENTRY_COST);
    }
}

/// Lets the object that lent the frame `phys`, charged to `budget`, go
/// of it while pages still use it: the frame and its charge then stay
/// until the last lend ends. Returns whether a page uses the frame;
/// otherwise the caller frees it.
pub fn orphan(phys: u64, budget: Option<&Arc<Budget>>) -> bool {
    if LENT.load(Ordering::Acquire) == 0 {
        return false;
    }
    match LENDS.lock().get_mut(&phys) {
        Some(lend) => {
            lend.orphan = Some(budget.cloned());
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_frames_until_the_last_lend_ends() {
        // Addresses that no frame allocator gives out in host tests.
        let phys = 0x7fff_0000_0000;
        assert!(!is_lent(phys) && !orphan(phys, None));
        lend(phys).unwrap();
        lend(phys).unwrap();
        end(phys);
        assert!(is_lent(phys));
        assert!(orphan(phys, None));
        end(phys);
        assert!(!is_lent(phys));
    }
}
