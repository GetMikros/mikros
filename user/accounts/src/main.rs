//! The accounts service: a system service that keeps the users in the
//! file `users` of `/var/lib/accounts/` and provides
//! `mikros.session.Accounts`.

#![no_std]
#![no_main]

extern crate alloc;

mod serve;
mod store;

use alloc::rc::Rc;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{ProviderAsyncServer, ProviderRequest};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use store::Store;

mikros_rt::raw_main!(main);

/// The protocol the service provides.
const PROTOCOL: &str = "mikros.session.Accounts";
/// The directory of the service's data.
const DATA: &str = "/var/lib/accounts";

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
    let Some((fs, at)) = managed::take_entry(&mut managed.namespace, DATA) else {
        return PANIC_EXIT_CODE;
    };
    let fs = NodeAsyncClient::new(fs, reactor.clone());
    let Ok(store) = executor.block_on(Store::open(&fs, at, &reactor)) else {
        return PANIC_EXIT_CODE;
    };
    drop(fs);
    let store = Rc::new(store);
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.protocol == PROTOCOL && c.instance.is_none() {
                let end = ServerEnd::new(c.server);
                spawner.spawn(serve::serve(store.clone(), end, reactor.clone()));
            }
        }
    });
    PANIC_EXIT_CODE
}
