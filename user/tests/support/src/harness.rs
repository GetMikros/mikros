//! Running a system test's checks: each in turn until one fails, whose
//! name and failure go to standard error, and whose number is the exit
//! status that selftest reports.

use crate::{Failure, Own, Result};
use alloc::format;
use mikros_rt::Managed;
use mikros_rt::sys;

/// The `id` of standard error among the startup handles: descriptor 2,
/// of the pipe kind (MIKSPEC-0058.R3).
const STDERR_ID: u32 = 2 + (2 << 28);

/// A system test's check, by name.
pub type Check = (&'static str, fn(&Own) -> Result);

/// Runs `checks` in order in the process whose startup transfer is
/// `managed`, and returns the program's exit status: 0 when every check
/// passed, otherwise the number, from 1, of the first that failed, after
/// writing its name and failure to standard error.
pub fn run(managed: &Managed, checks: &[Check]) -> i64 {
    let own = Own::of(managed);
    for (number, (name, check)) in checks.iter().enumerate() {
        if let Err(Failure { what, status }) = check(&own) {
            let line = format!("{name} FAILED: {what} ({status})\n");
            if let Some(stderr) = managed.handles.iter().find(|h| h.id == STDERR_ID) {
                let mut written = 0;
                // SAFETY: the pointer and length describe `line`.
                unsafe {
                    sys::stream_write(
                        stderr.handle.raw(),
                        line.as_ptr(),
                        line.len() as u64,
                        &mut written,
                    )
                };
            }
            return number as i64 + 1;
        }
    }
    0
}
