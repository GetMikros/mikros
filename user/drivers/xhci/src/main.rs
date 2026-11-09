//! The xHCI driver: the USB host process of one xHCI controller.
//!
//! `Start` takes the controller over from firmware, resets it and
//! brings it up with its command ring and the event ring of interrupter
//! 0, which a message-signaled interrupt or else the INTx event serves.
//! `Stop` stops the controller before the driver ends.

#![no_std]
#![no_main]

extern crate alloc;

mod controller;
mod regs;
mod ring;
mod wait;

use alloc::rc::Rc;
use controller::Controller;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DriverAsyncServer,
    DriverReconnectResponse, DriverRequest, DriverStartRequest, DriverStartResponse,
    DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_storage::PciBus;

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;

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
    // The driver has no log handle: its messages go to the log service.
    mikros_rt::log::connect(&managed.header.directory);
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = Rc::new(DriverAsyncServer::new(end, reactor.clone()));
    let process = managed.header.process.raw();
    let mut controller: Option<Rc<Controller>> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if controller.is_some() => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor, &spawner).await {
                        Ok(started) => {
                            controller = Some(started);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    if let Some(controller) = &controller {
                        controller.stop().await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                // A restarted PCI bus driver gives a new bus channel.
                DriverRequest::Reconnect(request, responder) => match &controller {
                    Some(controller) => {
                        controller.pci.reconnect(request.bus);
                        responder.reply(s, Ok(DriverReconnectResponse {}))
                    }
                    None => responder.reply(s, Err(Status::BadState)),
                },
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotFound))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotFound))
                }
            };
        }
        0
    })
}

/// Brings up the controller that `request` gives, with its `DMA`
/// resource. Fails with
/// `INVALID_ARGS` without a bus channel or a `DMA` resource, with the
/// status of the bus driver when it refused a request, and with
/// `INTERNAL` when the controller could not be brought up.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<Rc<Controller>, Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let pci = PciBus::new(bus, reactor);
    Controller::start(pci, process, dma, reactor, spawner).await
}

/// The kind of `resource`, if it is one.
fn kind(resource: &OwnedHandle) -> Option<u32> {
    let mut info = [0u8; INFO_RESOURCE_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `info` and `len`.
    let status = unsafe {
        sys::object_get_info(
            resource.raw(),
            INFO_RESOURCE,
            info.as_mut_ptr(),
            info.len() as u64,
            &mut len,
        )
    };
    (status == mikros_rt::OK).then(|| u32::from_le_bytes([info[0], info[1], info[2], info[3]]))
}
