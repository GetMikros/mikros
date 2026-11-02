//! The bootloader's use of UEFI.

mod gather;

use alloc::string::{String, ToString};
use core::fmt::{Display, Write};
use core::time::Duration;
use uefi::{Status, boot, system};

/// A step that failed while boot services run.
pub struct Failure {
    step: &'static str,
    reason: String,
}

impl Failure {
    pub fn new(step: &'static str, reason: impl Display) -> Failure {
        Failure {
            step,
            reason: reason.to_string(),
        }
    }
}

/// How long a failure stays on the screen without a key press.
const FAILURE_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);

/// Shows `failure`, waits for a key press or `FAILURE_WAIT`, and gives the
/// status to return to the boot manager or firmware.
fn fail(failure: &Failure) -> Status {
    system::with_stdout(|out| {
        let _ = writeln!(
            out,
            "mikrosboot: {} failed: {}\r",
            failure.step, failure.reason
        );
        let _ = writeln!(out, "Press a key to continue.\r");
    });
    system::with_stdin(|input| {
        let _ = input.reset(false);
        let mut waited = Duration::ZERO;
        while waited < FAILURE_WAIT {
            if let Ok(Some(_)) = input.read_key() {
                break;
            }
            boot::stall(POLL);
            waited += POLL;
        }
    });
    Status::LOAD_ERROR
}

pub fn run() -> Status {
    match gather::gather() {
        Ok(_gathered) => fail(&Failure::new(
            "entering the kernel",
            "this bootloader cannot start a kernel yet",
        )),
        Err(failure) => fail(&failure),
    }
}
