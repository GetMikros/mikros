//! The temporary file system: a system service that keeps a file system
//! in memory and provides it as `mikros.fs.Filesystem`, and the budgets
//! that its files are charged to as `mikros.fs.TemporaryFilesystem`. The
//! directory connects to it only clients with the protocols' permission.

use crate::fs::Fs;
use alloc::rc::Rc;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{ProviderAsyncServer, ProviderRequest};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The protocols the service provides.
const PROTOCOL: &str = "mikros.fs.Filesystem";
const TEMPORARY: &str = "mikros.fs.TemporaryFilesystem";

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
            if c.instance.is_some() {
                continue;
            }
            if c.protocol == PROTOCOL {
                let end = ServerEnd::new(c.server);
                spawner.spawn(crate::fs::serve_filesystem(fs.clone(), end, c.client.user));
            } else if c.protocol == TEMPORARY {
                spawner.spawn(crate::budget::serve(fs.clone(), ServerEnd::new(c.server)));
            }
        }
    });
    PANIC_EXIT_CODE
}
