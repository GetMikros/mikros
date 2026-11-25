//! Frames that more than one page uses, such as those that anonymous
//! memory objects lend to paged ones with `pager_lend`: how many pages
//! use each, so that none is freed or given to another object while one
//! does.

use crate::memory_object::MemoryObject;
use crate::nodes::{self, Map};
use crate::object::budget::Budget;
use crate::status::Error;
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// A page that uses a lent frame as part of its lend group: the source's
/// slot, or the slot of a paged object that the frame was lent to. A
/// member is a hint that is checked when it is used: the slot may have
/// let go of the frame since.
struct Member {
    koid: u64,
    index: u64,
    mem: Weak<MemoryObject>,
}

/// The objects that use one frame because it was lent, which count as
/// one object.
struct Group {
    members: Vec<Member>,
    /// The group is being moved to a new frame, which the members' writers
    /// wait for, and no page joins it meanwhile.
    moving: bool,
    /// The group moved to another frame: nothing joins this one.
    retired: bool,
    /// How many kernel writes into the frame are under way, held by
    /// `hold`: no clone takes the frame while one is.
    held: u64,
}

impl Group {
    /// Adds the page `index` of `mem`, unless it is a member already.
    /// Fails with `NoMemory`, changing nothing, without room.
    fn add(&mut self, mem: &Arc<MemoryObject>, index: u64) -> Result<(), Error> {
        let koid = mem.koid();
        if self
            .members
            .iter()
            .any(|m| m.koid == koid && m.index == index)
        {
            return Ok(());
        }
        self.members.try_reserve(1).map_err(|_| Error::NoMemory)?;
        self.members.push(Member {
            koid,
            index,
            mem: Arc::downgrade(mem),
        });
        Ok(())
    }
}

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
    /// The budget that paid for this entry, which gets it back when the
    /// entry goes, `None` for an entry nothing was charged for.
    bookkeeping: Option<Arc<Budget>>,
    /// The budget that the page of the frame is charged to and that gets
    /// it back when the last use ends, `None` for a frame charged to
    /// nothing. It stays alive as long as the frame is used.
    charge: Option<Arc<Budget>>,
    /// The pages that use the frame as lent, once it was.
    group: Option<Box<Group>>,
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

/// The kernel memory charged for the entry of a shared frame: the entry
/// in the table and its share of the tree's nodes.
pub(crate) const SHARE_COST: u64 = 2 * size_of::<(u64, Share)>() as u64;

/// The shared frames, by physical address.
static SHARES: SpinLock<Map<u64, Share>> =
    SpinLock::new(crate::lock_order::FRAME_SHARES, nodes::map());

/// How many frames `SHARES` holds, so that freeing frames while none is
/// shared need not look. A frame is shared with the sharing object's pages
/// locked, which they are again before that object can free the frame.
static SHARED: AtomicU64 = AtomicU64::new(0);

/// Records one more page of a paged object that uses the frame `phys` as a
/// lent frame, which its owner's slot, charged to `owner`, holds: page
/// `index` of `source`, the anchor of the lend group, when it is given. The
/// first time, the owner's use is recorded too, with its charge. Fails
/// with `NoMemory`, changing nothing, without room to record it.
pub fn lend(
    phys: u64,
    owner: Option<&Arc<Budget>>,
    anchor: Option<(&Arc<MemoryObject>, u64)>,
) -> Result<(), Error> {
    let mut shares = SHARES.lock();
    if let Some(share) = shares.get_mut(&phys) {
        if share
            .group
            .as_ref()
            .is_some_and(|group| group.moving || group.retired)
        {
            return Err(Error::BadState);
        }
        if let Some((source, index)) = anchor {
            match &mut share.group {
                Some(group) => group.add(source, index)?,
                None => {
                    let mut group = crate::fallible::try_box(Group {
                        members: Vec::new(),
                        moving: false,
                        retired: false,
                        held: 0,
                    })?;
                    group.add(source, index)?;
                    share.group = Some(group);
                }
            }
        }
        share.uses += 1;
        share.lent += 1;
        return Ok(());
    }
    let _claim = nodes::claim_insert()?;
    let group = match anchor {
        Some((source, index)) => {
            let mut group = crate::fallible::try_box(Group {
                members: Vec::new(),
                moving: false,
                retired: false,
                held: 0,
            })?;
            group.add(source, index)?;
            Some(group)
        }
        None => None,
    };
    shares.insert(
        phys,
        Share {
            uses: 2,
            lent: 1,
            copies: 0,
            bookkeeping: None,
            charge: owner.cloned(),
            group,
        },
    );
    SHARED.fetch_add(1, Ordering::Release);
    Ok(())
}

/// Records that page `index` of `mem` uses the frame `phys`, which a lend
/// of it counted, as a lent frame: a member of the frame's lend group.
/// Fails with `NoMemory`, changing nothing, without room.
pub fn landing(phys: u64, mem: &Arc<MemoryObject>, index: u64) -> Result<(), Error> {
    let mut shares = SHARES.lock();
    match shares.get_mut(&phys).and_then(|share| share.group.as_mut()) {
        // A group that moved or is moving cannot take the page: the
        // frame it has is not the group's.
        Some(group) if group.moving || group.retired => Err(Error::BadState),
        Some(group) => group.add(mem, index),
        None => Ok(()),
    }
}

/// How a write to a slot of the frame `phys` goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    /// The slot may write the frame, which no other object sees.
    InPlace,
    /// The slot gets a frame of its own first.
    Break,
    /// The slot is one of a lend group, whose clones see the frame: the
    /// whole group moves to a new frame first.
    Group,
}

/// What starting a kernel write into a slot of the frame `phys` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Nothing to count: nobody shares the frame as a group.
    Plain,
    /// The write is counted, until `unhold`.
    Counted,
    /// A clone uses the frame: the write moves the group first.
    Moved,
}

/// Starts a kernel write into the slot, not a clone's, that has the
/// frame `phys`, which pins the object. A clone does not take the frame of
/// a lend group while one is under way.
pub fn hold(phys: u64) -> Hold {
    if SHARED.load(Ordering::Acquire) == 0 {
        return Hold::Plain;
    }
    let mut shares = SHARES.lock();
    let Some(share) = shares.get_mut(&phys) else {
        return Hold::Plain;
    };
    let copies = share.copies;
    match share.group.as_mut() {
        None => Hold::Plain,
        Some(_) if copies > 0 => Hold::Moved,
        Some(group) => {
            group.held += 1;
            Hold::Counted
        }
    }
}

/// Ends a kernel write that `hold` counted.
pub fn unhold(phys: u64) {
    let mut shares = SHARES.lock();
    if let Some(group) = shares.get_mut(&phys).and_then(|s| s.group.as_mut()) {
        group.held = group.held.saturating_sub(1);
    }
}

/// How a write to a shared slot of the frame `phys` goes, `copy` when the
/// slot is one of an object made by `mem_clone`.
pub fn write_class(phys: u64, copy: bool) -> Write {
    if SHARED.load(Ordering::Acquire) == 0 {
        return Write::InPlace;
    }
    let shares = SHARES.lock();
    let Some(share) = shares.get(&phys) else {
        return Write::InPlace;
    };
    if copy {
        return if share.uses > 1 {
            Write::Break
        } else {
            Write::InPlace
        };
    }
    match (share.copies, share.lent) {
        (0, _) => Write::InPlace,
        (_, 0) => Write::Break,
        _ => Write::Group,
    }
}

/// What starting to move a lend group found.
pub enum Begin {
    /// The frame has no group with clones any more: nothing to move.
    Gone,
    /// Another move of the frame is under way.
    Busy,
    /// The move begins, and the frame's page is charged to the budget.
    Ready(Option<Arc<Budget>>),
}

/// Begins the move of the lend group of the frame `phys`, which clones
/// use too, to a new frame.
pub fn begin_move(phys: u64) -> Begin {
    let mut shares = SHARES.lock();
    let Some(share) = shares.get_mut(&phys) else {
        return Begin::Gone;
    };
    if share.copies == 0 || share.lent == 0 {
        return Begin::Gone;
    }
    let charge = share.charge.clone();
    match share.group.as_mut() {
        Some(group) if group.retired => Begin::Gone,
        Some(group) if group.moving => Begin::Busy,
        Some(group) => {
            group.moving = true;
            Begin::Ready(charge)
        }
        None => Begin::Gone,
    }
}

/// Ends the move of the group of the frame `phys`: when `moved`, the
/// group lives on another frame and this one is its no more.
pub fn end_move(phys: u64, moved: bool) {
    let mut shares = SHARES.lock();
    if let Some(group) = shares.get_mut(&phys).and_then(|s| s.group.as_mut()) {
        group.moving = false;
        if moved {
            group.retired = true;
            group.members.clear();
        }
    }
}

/// Takes one member that did not move off the new frame `new` after all,
/// of the kind `kind`. Returns whether none is left, in which case the
/// entry went and the caller frees the frame and gives its page back.
pub fn skip_moved(new: u64, kind: Kind) -> bool {
    let mut shares = SHARES.lock();
    let Some(share) = shares.get_mut(&new) else {
        // The entry went when the last of the others let go of the frame,
        // as the owner alone holds it then: and the owner is the one that
        // did not move, so nobody holds it.
        return kind == Kind::Owner;
    };
    share.uses -= 1;
    if kind == Kind::Lent {
        share.lent -= 1;
    }
    if share.uses > 0 {
        return false;
    }
    SHARED.fetch_sub(1, Ordering::Release);
    shares.remove(&new);
    true
}

/// Makes the entry of the frame `new`, which the group moves to: `uses`
/// pages use it, `lent` of them as lent, charged to `charge`, with the
/// members `members`. Fails with `NoMemory`, changing nothing.
pub fn create_moved(
    new: u64,
    charge: Option<Arc<Budget>>,
    uses: u64,
    lent: u64,
    members: &[(Arc<MemoryObject>, u64)],
) -> Result<(), Error> {
    let mut group = crate::fallible::try_box(Group {
        members: Vec::new(),
        moving: false,
        retired: false,
        held: 0,
    })?;
    for (mem, index) in members {
        group.add(mem, *index)?;
    }
    let mut shares = SHARES.lock();
    let _claim = nodes::claim_insert()?;
    shares.insert(
        new,
        Share {
            uses,
            lent,
            copies: 0,
            bookkeeping: None,
            charge,
            group: Some(group),
        },
    );
    SHARED.fetch_add(1, Ordering::Release);
    Ok(())
}

/// Forgets the pages of the object `koid` as members of the lend group
/// of the frame `phys`, once none of its pages uses the frame.
pub fn forget_members(phys: u64, koid: u64) {
    if SHARED.load(Ordering::Acquire) == 0 {
        return;
    }
    let mut shares = SHARES.lock();
    if let Some(group) = shares.get_mut(&phys).and_then(|s| s.group.as_mut()) {
        group.members.retain(|m| m.koid != koid);
    }
}

/// The pages that may use the frame `phys` as one lent to a paged object
/// or as the source that lent it, as `(object, page)` pairs to check.
/// Empty for a frame that was not lent. Fails with `NoMemory`.
pub fn members(phys: u64) -> Result<Vec<(Arc<MemoryObject>, u64)>, Error> {
    let mut snapshot: Vec<(Weak<MemoryObject>, u64)> = Vec::new();
    loop {
        let shares = SHARES.lock();
        let Some(group) = shares.get(&phys).and_then(|share| share.group.as_ref()) else {
            return Ok(Vec::new());
        };
        // The room is made with the table unlocked, and checked again.
        if snapshot.capacity() >= group.members.len() {
            snapshot.extend(group.members.iter().map(|m| (m.mem.clone(), m.index)));
            break;
        }
        let wanted = group.members.len();
        drop(shares);
        snapshot
            .try_reserve_exact(wanted)
            .map_err(|_| Error::NoMemory)?;
    }
    let mut found = Vec::new();
    found
        .try_reserve_exact(snapshot.len())
        .map_err(|_| Error::NoMemory)?;
    found.extend(
        snapshot
            .into_iter()
            .filter_map(|(mem, index)| mem.upgrade().map(|mem| (mem, index))),
    );
    Ok(found)
}

/// Records one more slot of an object made by `mem_clone` that uses the
/// frame `phys`, which its owner's slot, charged to `owner`, holds. The
/// first time, the owner's use is recorded too, with its charge, and the
/// entry is charged to `caller`, the budget of the call, which gets it
/// back when the entry goes. Fails with `NoMemory`, or `NoResources`
/// without room in `caller`, changing nothing.
pub fn copy(
    phys: u64,
    owner: Option<&Arc<Budget>>,
    caller: Option<&Arc<Budget>>,
) -> Result<(), Error> {
    let mut shares = SHARES.lock();
    if let Some(share) = shares.get_mut(&phys) {
        // A kernel write into the frame that is under way finishes first.
        if share.group.as_ref().is_some_and(|group| group.held > 0) {
            return Err(Error::BadState);
        }
        share.uses += 1;
        share.copies += 1;
        return Ok(());
    }
    let _claim = nodes::claim_insert()?;
    if let Some(caller) = caller {
        caller.charge_kernel(SHARE_COST)?;
    }
    shares.insert(
        phys,
        Share {
            uses: 2,
            lent: 0,
            copies: 1,
            bookkeeping: caller.cloned(),
            charge: owner.cloned(),
            group: None,
        },
    );
    SHARED.fetch_add(1, Ordering::Release);
    Ok(())
}

/// How many slots and orphans use the frame `phys`, 0 when it is not
/// shared.
pub fn uses(phys: u64) -> u64 {
    if SHARED.load(Ordering::Acquire) == 0 {
        return 0;
    }
    SHARES.lock().get(&phys).map_or(0, |share| share.uses)
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
    let (released, refund) = {
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
        let held = share.group.as_ref().is_some_and(|group| group.held > 0);
        if share.uses > 1 || share.copies > 0 || share.lent > 0 || held {
            return Released::Used;
        }
        if share.uses == 1 {
            // Only the owner's slot is left: the frame is not shared any
            // more, and the owner's slot holds its page and its charge as
            // any slot does. The entry goes, and its charge with it.
            SHARED.fetch_sub(1, Ordering::Release);
            let share = shares.remove(&phys).expect("the entry was just found");
            (Released::Used, share.bookkeeping)
        } else {
            SHARED.fetch_sub(1, Ordering::Release);
            let share = shares.remove(&phys).expect("the entry was just found");
            (Released::Last(share.charge), share.bookkeeping)
        }
    };
    // The entry's charge goes back with no lock held.
    if let Some(budget) = refund {
        budget.release_kernel(SHARE_COST);
    }
    released
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
        lend(A, None, None).unwrap();
        lend(A, None, None).unwrap();
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
        lend(A, None, None).unwrap();
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        // The owner still holds the frame, which is shared no longer, so
        // it has no entry.
        assert!(!is_lent(A));
        assert!(matches!(release(A, Kind::Owner), Released::Alone));
    }

    #[test]
    fn hands_the_charge_to_the_last_use() {
        let _lock = crate::arch::mock::lock();
        let budget = Budget::root();
        lend(A, Some(&budget), None).unwrap();
        // A later lend does not change who is charged.
        let other = Budget::root();
        lend(A, Some(&other), None).unwrap();
        assert!(matches!(release(A, Kind::Owner), Released::Used));
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        let Released::Last(Some(charge)) = release(A, Kind::Lent) else {
            panic!("the last use returns the charge");
        };
        assert!(Arc::ptr_eq(&charge, &budget));
    }

    #[test]
    fn frees_a_moved_frame_that_nobody_holds_when_the_anchor_did_not_move() {
        let _lock = crate::arch::mock::lock();
        // Two uses were counted for the move: the anchor and one lender.
        create_moved(A, None, 2, 1, &[]).unwrap();
        // The lender went again, which left the owner alone with the
        // frame, and the entry with it.
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        assert!(!is_lent(A));
        // The anchor did not move: nobody holds the frame.
        assert!(skip_moved(A, Kind::Owner));
        // A lender that did not move, with the owner on the frame, leaves
        // it alone.
        create_moved(B, None, 2, 1, &[]).unwrap();
        assert!(!skip_moved(B, Kind::Lent));
        assert!(matches!(release(B, Kind::Owner), Released::Last(None)));
    }

    #[test]
    fn counts_the_slots_of_clones_apart_from_lends() {
        let _lock = crate::arch::mock::lock();
        assert!(!has_copies(A));
        lend(A, None, None).unwrap();
        // Lends alone are no copies: the owner and they are one object.
        assert!(is_lent(A) && !has_copies(A));
        copy(A, None, None).unwrap();
        assert!(has_copies(A) && is_lent(A));
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        assert!(has_copies(A) && !is_lent(A));
        assert!(matches!(release(A, Kind::Copy), Released::Used));
        assert!(!has_copies(A));
        assert!(matches!(release(A, Kind::Owner), Released::Alone));
        // A first copy of an unshared frame records the owner's use too,
        // and the copy outlives the owner.
        copy(B, None, None).unwrap();
        assert!(matches!(release(B, Kind::Owner), Released::Used));
        assert!(matches!(release(B, Kind::Copy), Released::Last(None)));
    }

    #[test]
    fn keeps_the_frames_of_other_addresses_apart() {
        let _lock = crate::arch::mock::lock();
        lend(A, None, None).unwrap();
        lend(B, None, None).unwrap();
        assert!(matches!(release(A, Kind::Lent), Released::Used));
        assert!(matches!(release(A, Kind::Owner), Released::Alone));
        assert!(!is_lent(A) && is_lent(B));
        assert!(matches!(release(B, Kind::Lent), Released::Used));
        assert!(matches!(release(B, Kind::Owner), Released::Alone));
    }

    #[test]
    fn frees_the_frame_and_the_page_charge_when_the_last_lend_ends() {
        let _lock = crate::arch::mock::lock();
        let arena = crate::arch::mock::Arena::new(8);
        let budget = Budget::root();
        budget.charge_pages(1).unwrap();
        let phys = crate::frames::alloc_frame().unwrap();
        let free = arena.free();
        lend(phys, Some(&budget), None).unwrap();
        // The owner lets go first: the frame and its page stay.
        assert!(matches!(release(phys, Kind::Owner), Released::Used));
        assert_eq!((arena.free(), budget.usage().1), (free, 1));
        end(phys, Kind::Lent);
        assert_eq!((arena.free(), budget.usage().1), (free + 1, 0));
    }
}
