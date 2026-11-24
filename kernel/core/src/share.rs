//! Frames that more than one page uses, such as those that anonymous
//! memory objects lend to paged ones with `pager_lend`: how many pages
//! use each, so that none is freed or given to another object while one
//! does.

use crate::nodes::{self, Map};
use crate::object::budget::Budget;
use crate::object::memory::PAGE_ENTRY_COST;
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// A shared frame.
struct Share {
    /// The pages of paged objects that use the frame as one lent to them.
    uses: u64,
    /// Once the object that lent the frame has let go of it, the budget
    /// that its charge goes back to when the last lend ends, `None` for a
    /// frame charged to nothing.
    orphan: Option<Option<Arc<Budget>>>,
}

/// The shared frames, by physical address.
static SHARES: SpinLock<Map<u64, Share>> =
    SpinLock::new(crate::lock_order::FRAME_SHARES, nodes::map());

/// How many frames `SHARES` holds, so that freeing frames while none is
/// shared need not look. A frame is shared with the sharing object's pages
/// locked, which they are again before that object can free the frame.
static SHARED: AtomicU64 = AtomicU64::new(0);

/// Records one more page that uses the frame `phys`. Fails with
/// `NoMemory`, changing nothing, without room to record it.
pub fn add_use(phys: u64) -> Result<(), Error> {
    let mut shares = SHARES.lock();
    if let Some(share) = shares.get_mut(&phys) {
        share.uses += 1;
        return Ok(());
    }
    let _claim = nodes::claim_insert()?;
    shares.insert(
        phys,
        Share {
            uses: 1,
            orphan: None,
        },
    );
    SHARED.fetch_add(1, Ordering::Release);
    Ok(())
}

/// Whether a page uses the frame `phys` as a shared frame.
pub fn is_shared(phys: u64) -> bool {
    SHARED.load(Ordering::Acquire) != 0 && SHARES.lock().contains_key(&phys)
}

/// Ends one use of the frame `phys`. When it was the last and the object
/// that owned the frame has let go of it, the frame is freed and its
/// charge released.
pub fn end_use(phys: u64) {
    let orphan = {
        let mut shares = SHARES.lock();
        let Some(share) = shares.get_mut(&phys) else {
            return;
        };
        share.uses -= 1;
        if share.uses > 0 {
            return;
        }
        SHARED.fetch_sub(1, Ordering::Release);
        shares.remove(&phys).and_then(|share| share.orphan)
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

/// Lets the object that owned the frame `phys`, charged to `budget`, go
/// of it while pages still use it: the frame and its charge then stay
/// until the last use ends. Returns whether a page uses the frame;
/// otherwise the caller frees it.
pub fn orphan(phys: u64, budget: Option<&Arc<Budget>>) -> bool {
    if SHARED.load(Ordering::Acquire) == 0 {
        return false;
    }
    match SHARES.lock().get_mut(&phys) {
        Some(share) => {
            share.orphan = Some(budget.cloned());
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_frames_until_the_last_use_ends() {
        // The last use frees the frame with the frame allocator, which
        // other tests install in an arena.
        let _lock = crate::arch::mock::lock();
        // Addresses that no frame allocator gives out in host tests.
        let phys = 0x7fff_0000_0000;
        assert!(!is_shared(phys) && !orphan(phys, None));
        add_use(phys).unwrap();
        add_use(phys).unwrap();
        end_use(phys);
        assert!(is_shared(phys));
        assert!(orphan(phys, None));
        end_use(phys);
        assert!(!is_shared(phys));
    }

    #[test]
    fn keeps_the_frames_of_other_addresses_apart() {
        let _lock = crate::arch::mock::lock();
        let (a, b) = (0x7fff_1000_0000, 0x7fff_2000_0000);
        add_use(a).unwrap();
        add_use(b).unwrap();
        end_use(a);
        assert!(!is_shared(a) && is_shared(b));
        // Ending a use of a frame that is not shared changes nothing.
        end_use(a);
        assert!(is_shared(b));
        end_use(b);
        assert!(!is_shared(b));
    }
}
