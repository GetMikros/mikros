//! Frames that anonymous memory objects lend to paged ones with
//! `pager_lend`: the lends are uses of shared frames, which `share` counts.

use crate::object::budget::Budget;
use crate::status::Error;
use alloc::sync::Arc;

/// Records one more page that uses the frame `phys` as a lent frame.
/// Fails with `NoMemory`, changing nothing, without room to record it.
pub fn lend(phys: u64) -> Result<(), Error> {
    crate::share::add_use(phys)
}

/// Whether a page uses the frame `phys` as a lent frame.
pub fn is_lent(phys: u64) -> bool {
    crate::share::is_shared(phys)
}

/// Ends one lend of the frame `phys`. When it was the last and the object
/// that lent the frame has let go of it, the frame is freed and its
/// charge released.
pub fn end(phys: u64) {
    crate::share::end_use(phys);
}

/// Lets the object that lent the frame `phys`, charged to `budget`, go
/// of it while pages still use it: the frame and its charge then stay
/// until the last lend ends. Returns whether a page uses the frame;
/// otherwise the caller frees it.
pub fn orphan(phys: u64, budget: Option<&Arc<Budget>>) -> bool {
    crate::share::orphan(phys, budget)
}
