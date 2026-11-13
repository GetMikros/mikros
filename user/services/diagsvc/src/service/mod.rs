//! The diagnostic service of the diagnostic build. At start it makes a
//! `DIAG` resource from the root resource of its `root_resource` boot
//! handle and closes that handle. It serves `mikros.diag.Overview` to
//! processes that hold `diag.read`: `Snapshot` takes the kernel's account
//! of its state with `diag_snapshot` and gives it as text tables, with
//! the identity of each process that the process manager reports.
//!
//! In other builds the program only exits: their images do not hold it.

#[cfg(mikros_diag)]
mod serve;

use mikros_rt::sys;

mikros_rt::raw_main!(main);

#[cfg(mikros_diag)]
fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    serve::run(bootstrap)
}

#[cfg(not(mikros_diag))]
fn main(_bootstrap: sys::Handle, _arg2: u64) -> i64 {
    mikros_rt::PANIC_EXIT_CODE
}
