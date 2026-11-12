//! The bootloader's use of UEFI.

mod enter;
mod gather;
mod pci;

use core::fmt::{Arguments, Write};
use core::time::Duration;
pub use mikrosboot::report::Failure;
use mikrosboot::report::{self, Console};
use uefi::{Status, boot, system};

/// The UEFI console.
struct Uefi;

impl Console for Uefi {
    fn write_line(&mut self, line: Arguments) {
        system::with_stdout(|out| {
            let _ = writeln!(out, "{line}\r");
        });
    }

    fn reset_input(&mut self) {
        system::with_stdin(|input| {
            let _ = input.reset(false);
        });
    }

    fn key_pressed(&mut self) -> bool {
        system::with_stdin(|input| matches!(input.read_key(), Ok(Some(_))))
    }

    fn stall(&mut self, time: Duration) {
        boot::stall(time);
    }
}

/// Shows `failure`, waits for a key press or `report::FAILURE_WAIT`, and
/// gives the status to return to the boot manager or firmware.
fn fail(failure: &Failure) -> Status {
    report::report(&mut Uefi, failure);
    Status::LOAD_ERROR
}

pub fn run() -> Status {
    match gather::gather() {
        Ok(gathered) => fail(&enter::boot(gathered)),
        Err(failure) => fail(&failure),
    }
}
