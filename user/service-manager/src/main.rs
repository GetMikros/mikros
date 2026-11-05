//! The service manager.
//!
//! While it receives its startup transfer, it accepts the process
//! manager's `Bind` requests on the control endpoint that arrives as the
//! `USER` startup handle with ID 0. The connections it binds are kept
//! open, and nothing serves them yet. Once started, it keeps init's boot
//! handles, logs through the log among them, and reads the service
//! manifests of its boot file system.

#![no_std]
#![no_main]

extern crate alloc;

mod broker;
mod files;
mod services;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Directory, ProcessDirectory, ProcessDirectoryAsyncServer,
    ProcessDirectoryBindResponse, ProcessDirectoryRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, log, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the control endpoint among the `USER` startup handles.
const CONTROL_ID: u32 = 0;
/// The `id`s of init's boot handles among the `USER` startup handles.
const ROOT_RESOURCE_ID: u32 = 1;
const LOG_ID: u32 = 2;
const ROOT_BUDGET_ID: u32 = 3;
const FRAMEBUFFER_ID: u32 = 4;
const PLATFORM_INFO_ID: u32 = 5;

/// The `Directory` connections bound so far, with the identities they
/// are bound to.
type Bindings = Rc<RefCell<Vec<(ClientIdentity, ServerEnd<Directory>)>>>;

/// The handles init gave the service manager for the services that list
/// them.
#[expect(dead_code, reason = "given to services once they start")]
pub struct BootHandles {
    root_resource: Option<OwnedHandle>,
    log: Option<OwnedHandle>,
    root_budget: Option<OwnedHandle>,
    framebuffer: Option<OwnedHandle>,
    platform_info: Option<OwnedHandle>,
}

impl BootHandles {
    /// Takes init's boot handles from `handles`.
    fn take(handles: &mut Vec<StartupHandle>) -> BootHandles {
        let mut take = |id| {
            let at = handles
                .iter()
                .position(|h| h.role == HandleRole::User && h.id == id)?;
            Some(handles.remove(at).handle)
        };
        BootHandles {
            root_resource: take(ROOT_RESOURCE_ID),
            log: take(LOG_ID),
            root_budget: take(ROOT_BUDGET_ID),
            framebuffer: take(FRAMEBUFFER_ID),
            platform_info: take(PLATFORM_INFO_ID),
        }
    }
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let bindings = Bindings::default();
    let control_reactor = reactor.clone();
    let control_bindings = bindings.clone();
    let control_spawner = spawner.clone();
    let mut control = Some(move |handles: &mut Vec<StartupHandle>| {
        let at = handles
            .iter()
            .position(|h| h.role == HandleRole::User && h.id == CONTROL_ID);
        if let Some(at) = at {
            let end = handles.remove(at).handle;
            control_spawner.spawn(serve_control(end, control_reactor, control_bindings));
        }
    });
    let received = executor.block_on(managed::receive_async(
        bootstrap,
        reactor.clone(),
        |handles| {
            if let Some(take) = control.take() {
                take(handles);
            }
        },
    ));
    let Ok(mut managed) = received else {
        // Dropping the partial state closed every handle received.
        return PANIC_EXIT_CODE;
    };
    let boot = BootHandles::take(&mut managed.handles);
    if let Some(log) = &boot.log {
        log::set_handle(log.raw());
    }
    let root = managed
        .namespace
        .iter()
        .position(|e| e.path == "/")
        .map(|at| managed.namespace.swap_remove(at).directory);
    match root {
        Some(root) => {
            let root = NodeAsyncClient::new(root, reactor.clone());
            let header = managed.header;
            let me = ClientIdentity {
                pid: header.pid,
                user: header.user,
                app_id: header.app_id,
            };
            let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
            spawner.spawn(services::start(root, reactor, boot, launcher, me));
        }
        None => mikros_rt::log!(Error, "the namespace has no root"),
    }
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
