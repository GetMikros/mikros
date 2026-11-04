//! The service manager.
//!
//! For now it only receives its startup transfer and, while it does,
//! accepts the process manager's `Bind` requests on the control endpoint
//! that arrives as the `USER` startup handle with ID 0. The connections
//! it binds are kept open, and nothing serves them yet.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, ProcessDirectory, ProcessDirectoryAsyncServer,
    ProcessDirectoryBindResponse, ProcessDirectoryRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the control endpoint among the `USER` startup handles.
const CONTROL_ID: u32 = 0;

/// The `Directory` connections bound so far, with the identities they
/// are bound to.
type Bindings = Rc<RefCell<Vec<(ClientIdentity, ServerEnd<Directory>)>>>;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let bindings = Bindings::default();
    let control_reactor = reactor.clone();
    let control_bindings = bindings.clone();
    let mut control = Some(move |handles: &mut Vec<StartupHandle>| {
        let at = handles
            .iter()
            .position(|h| h.role == HandleRole::User && h.id == CONTROL_ID);
        if let Some(at) = at {
            let end = handles.remove(at).handle;
            spawner.spawn(serve_control(end, control_reactor, control_bindings));
        }
    });
    let received = executor.block_on(managed::receive_async(bootstrap, reactor, |handles| {
        if let Some(take) = control.take() {
            take(handles);
        }
    }));
    let Ok(_managed) = received else {
        // Dropping the partial state closed every handle received.
        return PANIC_EXIT_CODE;
    };
    // The control endpoint stays served.
    executor.block_on(core::future::pending::<()>());
    PANIC_EXIT_CODE
}

/// Accepts `Bind` requests on the control endpoint `end` until the
/// process manager closes it.
async fn serve_control(end: OwnedHandle, reactor: Reactor, bindings: Bindings) {
    let server = ProcessDirectoryAsyncServer::new(ServerEnd::<ProcessDirectory>::new(end), reactor);
    while let Ok(ProcessDirectoryRequest::Bind(request, responder)) = server.next().await {
        // The binding is active once it is recorded, before the reply.
        bindings
            .borrow_mut()
            .push((request.client, request.directory));
        let _ = responder.reply(server.server(), Ok(ProcessDirectoryBindResponse {}));
    }
}
