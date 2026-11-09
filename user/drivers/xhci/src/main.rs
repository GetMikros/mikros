//! The xHCI driver: the USB host process of one xHCI controller.
//!
//! `Start` maps the controller's registers, takes the controller over
//! from firmware, stops it and resets it. `Stop` stops the controller
//! before the driver ends.

#![no_std]
#![no_main]

extern crate alloc;

mod regs;
mod wait;

use alloc::rc::Rc;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DriverAsyncServer, DriverReconnectResponse, DriverRequest, DriverStartRequest,
    DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_storage::{Mmio, PciBus};
use regs::Regs;

mikros_rt::raw_main!(main);

const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
const KIND_DMA: u32 = 4;

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
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
    let mut controller: Option<(Regs, Rc<PciBus>)> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if controller.is_some() => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor).await {
                        Ok(started) => {
                            controller = Some(started);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    if let Some((regs, _)) = &controller {
                        let _ = regs.halt(&reactor).await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                // A restarted PCI bus driver gives a new bus channel.
                DriverRequest::Reconnect(request, responder) => match &controller {
                    Some((_, pci)) => {
                        pci.reconnect(request.bus);
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

fn bus<T>(
    reply: Result<Result<T, Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Status> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::Internal),
    }
}

/// Maps BAR 0 of the controller that `request` gives, takes the
/// controller over from firmware, stops it and resets it. Fails with
/// `INVALID_ARGS` without a bus channel or a `DMA` resource, with the
/// status of the bus driver when it refused a request, and with
/// `INTERNAL` when the controller did not stop or reset in time.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
) -> Result<(Regs, Rc<PciBus>), Status> {
    let bus_channel = request.bus.ok_or(Status::InvalidArgs)?;
    request
        .resources
        .iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let pci = PciBus::new(bus_channel, reactor);
    let device = pci.get();
    let bar = bus(device.get_bar(0).await)?.bar;
    let regs = Regs::new(Mmio::map(process, &bar.resource, bar.base, bar.size)?);
    if !regs.take_over(reactor).await {
        mikros_rt::log!(Warning, "xhci: firmware kept the controller; taking it anyway");
    }
    let failed = |_| Status::Internal;
    regs.halt(reactor).await.map_err(failed)?;
    regs.reset(reactor).await.map_err(failed)?;
    Ok((regs, pci))
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
