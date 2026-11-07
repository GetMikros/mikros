//! The temporary file system: a system service that keeps a file system
//! in memory and provides it as `mikros.fs.Filesystem`. The directory
//! connects to it only clients with the protocol's permission.

#![no_std]
#![no_main]

extern crate alloc;

mod file;
mod fs;
mod node;
mod watch;

use alloc::rc::Rc;
use fs::Fs;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{ProviderAsyncServer, ProviderRequest};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The protocol the service provides.
const PROTOCOL: &str = "mikros.fs.Filesystem";

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
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let provider = managed.handles.remove(at).handle;
    let fs = Rc::new(Fs::new(reactor.clone(), spawner.clone()));
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor);
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.protocol == PROTOCOL && c.instance.is_none() {
                let end = ServerEnd::new(c.server);
                spawner.spawn(fs::serve_filesystem(fs.clone(), end, c.client.user));
            }
        }
    });
    PANIC_EXIT_CODE
}
