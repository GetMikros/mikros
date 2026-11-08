// SPDX-License-Identifier: GPL-2.0
//! The btrfs server (MIKSPEC-0053.R1, MIKSPEC-0053.R2): Linux's btrfs
//! code on the btrfs shim, which `mikros-btrfs` links.
//!
//! On `Start` it starts the shim, with its host layer on the system
//! calls, but it cannot read partitions yet, so `Start` then fails with
//! `NOT_SUPPORTED`. `Stop` ends it. `ResetChild` and `ConnectChild` fail
//! with `NOT_SUPPORTED`.

#![no_std]
#![no_main]

extern crate alloc;

mod host;
mod time;

use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DriverAsyncServer, DriverRequest, DriverStartRequest, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(mut managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = DriverAsyncServer::new(end, reactor);
    let process = managed.header.process.raw();
    let directory = DirectoryClient::new(managed.header.directory);
    let mut started = false;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if started => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    started = true;
                    let status = start(request, process, &directory);
                    responder.reply(s, Err(status))
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
            };
        }
        0
    })
}

/// How much memory the shim may hold: a quarter of the machine's, which
/// sizes its hash tables and the page cache's dirty limits.
fn memory() -> u64 {
    let mut record = [0u8; 16];
    // SAFETY: the record holds the 16 bytes the call writes.
    unsafe { sys::system_get_memory_info(record.as_mut_ptr()) };
    let frames = u64::from_le_bytes(record[..8].try_into().unwrap());
    (frames * 4096 / 4).max(64 << 20)
}

/// Starts the shim for the partition behind the `bus` channel of
/// `request`, which it cannot read yet: fails with `INVALID_ARGS`
/// without a bus channel, with `INTERNAL` when the shim cannot start,
/// and with `NOT_SUPPORTED` otherwise.
fn start(request: DriverStartRequest, process: sys::Handle, directory: &DirectoryClient) -> Status {
    if request.bus.is_none() {
        return Status::InvalidArgs;
    }
    host::init(
        process,
        memory(),
        time::page(directory, process).unwrap_or(0),
    );
    match mikros_btrfs::start() {
        Ok(()) => Status::NotSupported,
        Err(_) => Status::Internal,
    }
}
