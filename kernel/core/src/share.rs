//! Frames that more than one page uses, such as those that anonymous
//! memory objects lend to paged ones with `pager_lend`: how many pages
//! use each, so that none is freed or given to another object while one
//! does.

use crate::nodes::{self, Map};
use crate::object::budget::Budget;
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// A shared frame.
struct Share {
    /// The slots and orphans that use the frame: the object that owns it
    /// and the pages that use it.
    uses: u64,
    /// How many of those are pages of paged objects that use the frame as
    /// one lent to them.
    lent: u64,
    /// How many are slots of objects made by `mem_clone`, which see the
    /// frame's bytes as they were and are given a copy to write to. The
    /// owner and the lent pages share one frame whatever is written, as
    /// one object.
    copies: u64,
    /// The budget that the page of the frame is charged to and that gets
    /// it back when the last use ends, `None` for a frame charged to
    /// nothing. It stays alive as long as the frame is used.
    charge: Option<Arc<Budget>>,
}

/// The kind of a use of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The slot of the object that owns the frame, or of another that
    /// takes its place.
    Owner,
    /// A page of a paged object that uses the frame as one lent to it.
    Lent,
    /// The slot of an object made by `mem_clone`.
    Copy,
}

/// What ending one use of a frame leaves.
#[derive(Debug)]
pub enum Released {
    /// The frame is not shared: the slot owns it alone.
    Alone,
    /// Other uses remain.
    Used,
    /// It was the last use: the caller frees the frame and gives the page
    /// back to the budget it is charged to, if any.
    Last(Option<Arc<Budget>>),
}

/// The shared frames, by physical address.
static SHARES: SpinLock<Map<u64, Share>> =
    SpinLock::new(crate::lock_order::FRAME_SHARES, nodes::map());

/// How many frames `SHARES` holds, so that freeing frames while none is
/// shared need not look. A frame is shared with the sharing object's pages
/// locked, which they are again before that object can free the frame.
static SHARED: AtomicU64 = AtomicU64::new(0);

/// Records one more page of a paged object that uses the frame `phys` as a
/// lent frame, which its owner's slot, charged to `owner`, holds. The first
/// time, the owner's use is recorded too, with its charge. Fails with
/// `NoMemory`, changing nothing, without room to record it.
pub fn lend(phys: u64, owner: Option<&Arc<Budget>>) -> Result<(), Error> {
    let mut shares = SHARES.lock();
    if let Some(share) = shares.get_mut(&phys) {
        share.uses += 1;
        share.lent += 1;
        return Ok(());
    }
    let _claim = nodes::claim_insert()?;
    shares.insert(
        phys,
        Share {
            uses: 2,
            lent: 1,
            copies: 0,
            charge: owner.cloned(),
        },
    );
    SHARED.fetch_add(1, Ordering::Release);
    Ok(())
}

/// Records one more slot of an object made by `mem_clone` that uses the
/// frame `phys`, which its owner's slot, charged to `owner`, holds. The
/// first time, the owner's use is recorded too, with its charge. Fails
/// with `NoMemory`, changing nothing, without room to record it.
pub fn copy(phys: u64, owner: Option<&Arc<Budget>>) -> Result<(), Error> {
    let mut shares = SHARES.lock();
    if let Some(share) = shares.get_mut(&phys) {
        share.uses += 1;
        share.copies += 1;
        return Ok(());
    }
    let _claim = nodes::claim_insert()?;
    shares.insert(
        phys,
        Share {
            uses: 2,
            lent: 0,
            copies: 1,
            charge: owner.cloned(),
        },
    );
    SHARED.fetch_add(1, Ordering::Release);
    Ok(())
}

/// Whether a slot of an object made by `mem_clone` uses the frame `phys`,
/// so that a write to any other use of it needs a frame of its own.
pub fn has_copies(phys: u64) -> bool {
    SHARED.load(Ordering::Acquire) != 0
        && SHARES
            .lock()
            .get(&phys)
            .is_some_and(|share| share.copies > 0)
}

/// Whether a page of a paged object uses the frame `phys` as a lent
/// frame.
pub fn is_lent(phys: u64) -> bool {
    SHARED.load(Ordering::Acquire) != 0
        && SHARES.lock().get(&phys).is_some_and(|share| share.lent > 0)
}

/// Ends one use of the frame `phys`, of the kind `kind`.
pub fn release(phys: u64, kind: Kind) -> Released {
    if SHARED.load(Ordering::Acquire) == 0 {
        return Released::Alone;
    }
    let mut shares = SHARES.lock();
    let Some(share) = shares.get_mut(&phys) else {
        return Released::Alone;
    };
    share.uses -= 1;
    match kind {
        Kind::Owner => {}
        Kind::Lent => share.lent -= 1,
        Kind::Copy => share.copies -= 1,
    }
    if share.uses > 0 {
        return Released::Used;
    }
    SHARED.fetch_sub(1, Ordering::Release);
    let share = shares.remove(&phys).expect("the entry was just found");
    Released::Last(share.charge)
}

/// Ends one use of the frame `phys`, of the kind `kind`. When it was the
/// last use, the frame is freed and its page charge released.
pub fn end(phys: u64, kind: Kind) {
    if let Released::Last(charge) = release(phys, kind) {
        if let Some(allocator) = crate::frames::FRAMES.lock().as_mut() {
            allocator.free(phys, 1);
        }
        if let Some(budget) = charge {
            budget.release_pages(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Addresses that no frame allocator gives out in host tests, and the
    // lock that keeps them from a test that installs an arena.
    const A: u64 = 0x7fff_0000_0000;
    const B: u64 = 0x7fff_1000_0000;

    #[test]
    fn keeps_frames_until_the_last_use_ends() {
        let _lock = crate::arch::mock::lock();
        assert!(!is_lent(A) && matches!(release(A, Kind::Owner), Released::Alone));
        // Two lends of a frame its owner holds: three uses.
        lend(A, None).unwrap();
        lend(A, None).unwrap();
        assert!(is_lent(A));
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        assert!(matches!(release(A, Kind::Owner), Released::Used));
        assert!(is_lent(A));
        assert!(matches!(release(A, Kind::Lent), Released::Last(None)));
        assert!(!is_lent(A) && matches!(release(A, Kind::Lent), Released::Alone));
    }

    #[test]
    fn is_lent_only_while_a_page_uses_the_frame_as_lent() {
        let _lock = crate::arch::mock::lock();
        lend(A, None).unwrap();
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        // The owner still holds the frame, which is shared no longer.
        assert!(!is_lent(A));
        assert!(matches!(release(A, Kind::Owner), Released::Last(None)));
    }

    #[test]
    fn hands_the_charge_to_the_last_use() {
        let _lock = crate::arch::mock::lock();
        let budget = Budget::root();
        lend(A, Some(&budget)).unwrap();
        // A later lend does not change who is charged.
        let other = Budget::root();
        lend(A, Some(&other)).unwrap();
        assert!(matches!(release(A, Kind::Owner), Released::Used));
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        let Released::Last(Some(charge)) = release(A, Kind::Lent) else {
            panic!("the last use returns the charge");
        };
        assert!(Arc::ptr_eq(&charge, &budget));
    }

    #[test]
    fn counts_the_slots_of_clones_apart_from_lends() {
        let _lock = crate::arch::mock::lock();
        assert!(!has_copies(A));
        lend(A, None).unwrap();
        // Lends alone are no copies: the owner and they are one object.
        assert!(is_lent(A) && !has_copies(A));
        copy(A, None).unwrap();
        assert!(has_copies(A) && is_lent(A));
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        assert!(has_copies(A) && !is_lent(A));
        assert!(matches!(release(A, Kind::Copy), Released::Used));
        assert!(!has_copies(A));
        assert!(matches!(release(A, Kind::Owner), Released::Last(None)));
        // A first copy of an unshared frame records the owner's use too.
        copy(B, None).unwrap();
        assert!(matches!(release(B, Kind::Owner), Released::Used));
        assert!(matches!(release(B, Kind::Copy), Released::Last(None)));
    }

    #[test]
    fn keeps_the_frames_of_other_addresses_apart() {
        let _lock = crate::arch::mock::lock();
        lend(A, None).unwrap();
        lend(B, None).unwrap();
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        assert!(matches!(release(A, Kind::Owner), Released::Last(None)));
        assert!(!is_lent(A) && is_lent(B));
        assert!(matches!(release(B, Kind::Lent), Released::Used));
        assert!(matches!(release(B, Kind::Owner), Released::Last(None)));
    }

    #[test]
    fn frees_the_frame_and_the_page_charge_when_the_last_lend_ends() {
        let _lock = crate::arch::mock::lock();
        let arena = crate::arch::mock::Arena::new(8);
        let budget = Budget::root();
        budget.charge_pages(1).unwrap();
        let phys = crate::frames::alloc_frame().unwrap();
        let free = arena.free();
        lend(phys, Some(&budget)).unwrap();
        // The owner lets go first: the frame and its page stay.
        assert!(matches!(release(phys, Kind::Owner), Released::Used));
        assert_eq!((arena.free(), budget.usage().1), (free, 1));
        end(phys, Kind::Lent);
        assert_eq!((arena.free(), budget.usage().1), (free + 1, 0));
    }
}
