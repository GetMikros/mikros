//! The resets of a device whose stop it did not confirm. A driver that
//! asks for its device's reset because the device did not confirm a stop
//! counts the reset as the confirmation (MIKSPEC-0080.R12,
//! MIKSPEC-0025.R64), but a device that ignored the stop may also ignore
//! the reset and go on with the commands it had. Memory that was pinned
//! for the device before such a reset is therefore closed into the
//! quarantine (MIKSPEC-0080.R7) when it goes, rather than unpinned, and
//! never reused: the bus driver releases the quarantine at the next reset
//! of the device, once bus mastering is off (MIKSPEC-0026.R15).

use alloc::rc::Rc;
use core::cell::Cell;

/// How many resets of one device were not confirmed by the device. The
/// clones of one count the same resets.
#[derive(Clone, Default)]
pub struct Unconfirmed(Rc<Cell<u64>>);

impl Unconfirmed {
    /// Counts a reset that the device did not confirm, before the driver
    /// asks for it.
    pub fn note(&self) {
        self.0.set(self.0.get() + 1);
    }

    /// A mark of the resets so far, taken when memory is pinned or handed
    /// to the device.
    pub fn mark(&self) -> Mark {
        Mark(self.0.clone(), self.0.get())
    }
}

/// The resets that a device had not confirmed when the mark was taken.
pub struct Mark(Rc<Cell<u64>>, u64);

impl Mark {
    /// Whether a reset that the device did not confirm came since the
    /// mark was taken, so that the device may still reach the memory.
    pub fn passed(&self) -> bool {
        self.0.get() != self.1
    }
}

/// Lets go of the pin `pmt`: with `pmt_unpin` while the device cannot
/// reach its pages any more, or by closing the handle, which leaves the
/// pages in the quarantine, when `mark` says that a reset the device did
/// not confirm came since they were pinned.
pub fn release(pmt: mikros_rt::sys::Handle, mark: Option<&Mark>) {
    if mark.is_some_and(Mark::passed) {
        // SAFETY: the call takes no pointers; the caller owns the handle.
        unsafe { mikros_rt::sys::handle_close(pmt) };
    } else {
        // SAFETY: the call takes no pointers; it also closes the handle,
        // which the caller owns.
        unsafe { mikros_rt::sys::pmt_unpin(pmt) };
    }
}
