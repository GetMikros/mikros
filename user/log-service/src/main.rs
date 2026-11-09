//! The log service: a system service that collects the records of the
//! kernel log and those that clients write through `mikros.log.Writer`
//! into one log, kept in memory, and lets clients that hold `logs.read`
//! read it through `mikros.log.Reader`.
//!
//! It reads the kernel log with a `LogReader` on the `log_manage` boot
//! handle, from the oldest record the kernel still holds. A record that
//! a client writes takes its pid and app ID from the client's identity
//! as the directory gave it, and its message is also copied into the
//! kernel log, whose records from this process are not taken again.

#![no_std]
#![no_main]

extern crate alloc;

mod kernel;
mod log;
mod reader;
mod writer;

use crate::log::Log;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{ProviderAsyncServer, ProviderRequest};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The protocols the service provides.
const WRITER: &str = "mikros.log.Writer";
const READER: &str = "mikros.log.Reader";
/// The `id` of the `log_manage` boot handle among the `USER` startup
/// handles.
const LOG_MANAGE_ID: u32 = 2;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let Some(provider) = take(&mut managed.handles, |h| h.role == HandleRole::Provider) else {
        return PANIC_EXIT_CODE;
    };
    let kernel_log = take(&mut managed.handles, |h| {
        h.role == HandleRole::User && h.id == LOG_MANAGE_ID
    });
    let log = Rc::new(Log::new(kernel_log.as_ref().map(|h| h.raw())));
    match &kernel_log {
        Some(handle) => {
            mikros_rt::log::set_handle(handle.raw());
            let own = koid(managed.header.process.raw());
            spawner.spawn(kernel::read(
                log.clone(),
                reactor.clone(),
                handle.raw(),
                own,
            ));
        }
        None => mikros_rt::log!(Error, "log: there is no log_manage handle"),
    }
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.instance.is_some() {
                continue;
            }
            if c.protocol == WRITER {
                let end = ServerEnd::new(c.server);
                spawner.spawn(writer::serve(log.clone(), end, reactor.clone(), c.client));
            } else if c.protocol == READER {
                let end = ServerEnd::new(c.server);
                let task = reader::serve(log.clone(), end, reactor.clone(), spawner.clone());
                spawner.spawn(task);
            }
        }
    });
    // The kernel log's handle stays open while the program runs.
    drop(kernel_log);
    PANIC_EXIT_CODE
}

/// Takes the first startup handle that `wanted` picks out of `handles`.
fn take(
    handles: &mut Vec<StartupHandle>,
    wanted: impl Fn(&StartupHandle) -> bool,
) -> Option<OwnedHandle> {
    let at = handles.iter().position(wanted)?;
    Some(handles.remove(at).handle)
}

/// The koid of the object behind `handle`, or 0 when it cannot be read.
fn koid(handle: sys::Handle) -> u64 {
    const INFO_BASIC: u32 = 1;
    let mut record = [0u8; 24];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status =
        unsafe { sys::object_get_info(handle, INFO_BASIC, record.as_mut_ptr(), 24, &mut len) };
    if status != mikros_rt::OK {
        return 0;
    }
    u64::from_le_bytes(record[..8].try_into().expect("8 bytes"))
}
