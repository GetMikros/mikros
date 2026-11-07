//! The process manager.
//!
//! init starts it with a channel that carries one `Start` message and is
//! then closed. Only after that does the process manager serve init's
//! `InitBootstrap` requests.

#![no_std]
#![no_main]

extern crate alloc;

mod broker;
mod init;
mod launcher;
mod processes;
mod records;
mod start;

use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{Error, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::process::{
    ManagerBootstrapRequest, ManagerBootstrapServer, ManagerStartup,
};
use mikros_rt::{PANIC_EXIT_CODE, heap, log, sys};

mikros_rt::raw_main!(main);

/// The only version of `Start`.
const VERSION: u32 = 2;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Some(startup) = receive(bootstrap) else {
        return PANIC_EXIT_CODE;
    };
    let mut executor = match Executor::new() {
        Ok(executor) => executor,
        Err(status) => {
            mikros_rt::log!(Error, "creating the executor failed: {status}");
            return PANIC_EXIT_CODE;
        }
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let mut manager = match init::Manager::new(startup, reactor, spawner) {
        Ok(manager) => manager,
        Err(status) => {
            mikros_rt::log!(Error, "serving the boot image failed: {status}");
            return PANIC_EXIT_CODE;
        }
    };
    executor.block_on(async move {
        manager.serve_init().await;
        // The other tasks go on after init closes its end.
        core::future::pending::<()>().await;
    });
    PANIC_EXIT_CODE
}

/// Reads `Start` from `bootstrap` and waits until init closes it.
/// Returns `None`, having dropped every handle received, when the
/// message is not a valid version 2 `Start`, or when another message
/// follows it.
fn receive(bootstrap: sys::Handle) -> Option<ManagerStartup> {
    // SAFETY: the kernel gave the program this handle, and nothing else
    // owns it.
    let end = ServerEnd::new(unsafe { OwnedHandle::from_raw(bootstrap) });
    let server = ManagerBootstrapServer::new(end);
    let ManagerBootstrapRequest::Start(start) = server.next(i64::MAX).ok()?;
    let startup = start.startup;
    heap::set_process(startup.process.raw());
    log::set_handle(startup.log.raw());
    if startup.version != VERSION {
        return None;
    }
    match server.next(i64::MAX) {
        Err(Error::Closed) => Some(startup),
        _ => None,
    }
}
