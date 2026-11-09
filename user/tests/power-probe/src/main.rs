//! The boot service of `tests/power.img`, which the power-off run of
//! `cargo xtask test` boots. It logs that it runs, and then waits until
//! the service manager stops it, which the service manager does when it
//! shuts the machine down for the power button that the runner presses
//! through QEMU's monitor: the run checks that it is stopped first, as
//! the service started last.

#![no_std]
#![no_main]

extern crate alloc;

use mikros_rt::exec::Executor;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the `log_manage` boot handle among the `USER` startup
/// handles.
const LOG_ID: u32 = 2;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor, |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let log = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID);
    if let Some(log) = log {
        mikros_rt::log::set_handle(log.handle.raw());
    }
    mikros_rt::log!(Info, "power-probe: running until it is stopped");
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}
