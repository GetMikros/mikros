//! The service manager.
//!
//! While it receives its startup transfer, it accepts the process
//! manager's `Bind` requests on the control endpoint that arrives as the
//! `USER` startup handle with ID 0, and serves `Directory` on each
//! connection it binds, to the identity it is bound to. Once started, it
//! keeps init's boot handles, logs through the log among them, reads the
//! service manifests of its boot file system and starts the boot
//! services.

#![no_std]
#![no_main]

extern crate alloc;

mod boot;
mod broker;
mod directory;
mod files;
mod launch;
mod registrar;
mod services;

use crate::boot::BootHandles;
use crate::directory::Dir;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::NodeAsyncClient;
use mikros_rt::interfaces::mikros::process::{HandleRole, LauncherAsyncClient, StartupHandle};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, ProcessDirectory, ProcessDirectoryAsyncServer, ProcessDirectoryBindResponse,
    ProcessDirectoryRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, log, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the control endpoint among the `USER` startup handles.
const CONTROL_ID: u32 = 0;
fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let dir = Dir::new(reactor.clone(), spawner.clone());
    let control_reactor = reactor.clone();
    let control_dir = dir.clone();
    let control_spawner = spawner.clone();
    let mut control = Some(move |handles: &mut Vec<StartupHandle>| {
        let at = handles
            .iter()
            .position(|h| h.role == HandleRole::User && h.id == CONTROL_ID);
        if let Some(at) = at {
            let end = handles.remove(at).handle;
            control_spawner.spawn(serve_control(end, control_reactor, control_dir));
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
    let boot = Rc::new(BootHandles::take(&mut managed.handles));
    if let Some(log) = boot.log() {
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
            spawner.spawn(services::start(
                root,
                reactor,
                launcher,
                me,
                dir,
                boot.clone(),
            ));
        }
        None => mikros_rt::log!(Error, "the namespace has no root"),
    }
    // The control endpoint stays served, and the boot handles, the log
    // among them, stay open.
    executor.block_on(core::future::pending::<()>());
    drop(boot);
    PANIC_EXIT_CODE
}

/// Accepts `Bind` requests on the control endpoint `end` until the
/// process manager closes it.
async fn serve_control(end: OwnedHandle, reactor: Reactor, dir: Rc<Dir>) {
    let server = ProcessDirectoryAsyncServer::new(ServerEnd::<ProcessDirectory>::new(end), reactor);
    while let Ok(ProcessDirectoryRequest::Bind(request, responder)) = server.next().await {
        // The binding is active once it is served, before the reply.
        dir.serve(request.directory, request.client);
        let _ = responder.reply(server.server(), Ok(ProcessDirectoryBindResponse {}));
    }
}
