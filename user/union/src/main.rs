//! The union server: a system service that provides `mikros.fs.Union`,
//! whose views combine several directories into one. A name resolves to
//! the first layer that holds it, unless a layer before that one holds a
//! whiteout of it, and directories of a name merge across the layers.
//! Opens of what is not a directory go to the layer that holds it, so
//! that the client talks to that layer's server. Changes go to the
//! writable layer: what lower layers hold is copied up first, and names
//! removed that they still hold are whited out. Revocable views are not
//! there yet.

#![no_std]
#![no_main]

extern crate alloc;

mod change;
mod dir;
mod view;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::{
    Error, NodeAsyncClient, Union, UnionAsyncServer, UnionCreateRequest, UnionCreateResponse,
    UnionRequest,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{ProviderAsyncServer, ProviderRequest};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use view::{Dir, View};

mikros_rt::raw_main!(main);

/// The protocol the service provides.
const PROTOCOL: &str = "mikros.fs.Union";

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
    let provider = ProviderAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    executor.block_on(async {
        while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
            // Other protocols and named instances are not ours; dropping
            // the server end closes the connection.
            if c.protocol == PROTOCOL && c.instance.is_none() {
                let end = ServerEnd::new(c.server);
                spawner.spawn(serve(end, reactor.clone(), spawner.clone()));
            }
        }
    });
    PANIC_EXIT_CODE
}

/// Serves `Union` on `end`.
async fn serve(end: ServerEnd<Union>, reactor: Reactor, spawner: Spawner) {
    let server = UnionAsyncServer::new(end, reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            UnionRequest::Create(q, r) => r.reply(s, create(q, &reactor, &spawner)),
            UnionRequest::CreateRevocable(_, r) => r.reply(s, Err(Error::NotSupported)),
            UnionRequest::Revoke(_, r) => r.reply(s, Err(Error::NotSupported)),
        };
    }
}

/// Serves a view of the layers of `q` on its `view` channel. Fails with
/// `INVALID` without layers, or unless at most the first is writable.
fn create(
    q: UnionCreateRequest,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<UnionCreateResponse, Error> {
    let lower_writable = q.layers.iter().skip(1).any(|l| l.writable);
    if q.layers.is_empty() || lower_writable {
        return Err(Error::Invalid);
    }
    let writable = q.layers[0].writable;
    let roots: Vec<Rc<NodeAsyncClient>> = q
        .layers
        .into_iter()
        .map(|l| Rc::new(NodeAsyncClient::new(l.directory, reactor.clone())))
        .collect();
    let view = Rc::new(View {
        writable,
        rights: q.rights,
        roots,
        tokens: RefCell::new(BTreeMap::new()),
        reactor: reactor.clone(),
        spawner: spawner.clone(),
    });
    spawner.spawn(dir::serve(Dir::root(view), q.view, q.rights));
    Ok(UnionCreateResponse {})
}
