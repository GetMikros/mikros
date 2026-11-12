//! The basic display driver.
//!
//! It is the fallback driver of the device that has the boot framebuffer:
//! `platform:boot-framebuffer`, or the PCI function that provides it, with
//! `display:boot-framebuffer`. It never uses its bus channel.
//! It takes the framebuffer's memory object from its startup handle with
//! the role `USER` and ID 1, and its description from the device's
//! properties, and publishes its GPU device with the gpudev library,
//! which lets one client at a time, holding `gpu.display`, own the
//! display, and shows the system log on the framebuffer while none does.
//! It serves the `Display` part of `mikros.gpu` with one connector, CRTC
//! and primary plane in the framebuffer's mode, dumb buffers in
//! XRGB8888, and commits that it presents by copying the plane's buffer
//! into the framebuffer, converting each pixel to the framebuffer's
//! format.

#![no_std]
#![no_main]

extern crate alloc;

mod display;
mod framebuffer;

use alloc::rc::Rc;
use display::Display;
use framebuffer::{Description, Framebuffer};
use mikros_gpudev::GpuDevice;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverRequest, DriverStartRequest,
    DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the framebuffer's startup handle among the `USER` ones.
const FRAMEBUFFER_ID: u32 = 1;

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
    let mut take = |role: HandleRole, id: u32| {
        let at = managed
            .handles
            .iter()
            .position(|h| h.role == role && h.id == id)?;
        Some(managed.handles.remove(at).handle)
    };
    let Some(provider) = take(HandleRole::Provider, 0) else {
        return PANIC_EXIT_CODE;
    };
    let mut memory = take(HandleRole::User, FRAMEBUFFER_ID);
    let server = DriverAsyncServer::new(ServerEnd::new(provider), reactor.clone());
    let process = managed.header.process.raw();
    let directory = Rc::new(DirectoryAsyncClient::new(
        managed.header.directory,
        reactor.clone(),
    ));
    let mut running: Option<(Rc<GpuDevice<Display>>, DeviceManagerAsyncClient)> = None;
    // The bus channel that a PCI function's driver gets: never used, but
    // kept open, as the bus driver stops the function decoding once it
    // closes, which would leave the framebuffer unreachable.
    let mut bus = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(mut request, responder) => {
                    bus = request.bus.take();
                    let Some(memory) = memory.take() else {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    };
                    match start(request, memory, process, &directory, &reactor, &spawner).await {
                        Ok(r) => {
                            running = Some(r);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                // A GPU driver takes the display over: nothing is
                // presented or drawn once the framebuffer is unmapped.
                DriverRequest::Stop(_, responder) => {
                    if let Some((gpu, _)) = &running
                        && let Some(fb) = gpu.backend().framebuffer.borrow_mut().take()
                    {
                        fb.unmap(process);
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::PrepareShutdown(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
            };
        }
        PANIC_EXIT_CODE
    })
}

async fn start(
    request: DriverStartRequest,
    memory: OwnedHandle,
    process: sys::Handle,
    directory: &Rc<DirectoryAsyncClient>,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Rc<GpuDevice<Display>>, DeviceManagerAsyncClient), Status> {
    let description = Description::from(&request.device.properties).ok_or_else(|| {
        mikros_rt::log!(
            Error,
            "basicdisplay: the framebuffer's description is not valid"
        );
        Status::InvalidArgs
    })?;
    let framebuffer = Framebuffer::map(memory, description, process).ok_or_else(|| {
        mikros_rt::log!(Error, "basicdisplay: mapping the framebuffer failed");
        Status::NotSupported
    })?;
    let display = Rc::new(Display::new(
        framebuffer,
        process,
        reactor.clone(),
        spawner.clone(),
    ));
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let gpu = GpuDevice::publish(
        display,
        &manager,
        directory.clone(),
        reactor.clone(),
        spawner.clone(),
    )
    .await?;
    mikros_rt::log!(
        Info,
        "basicdisplay: serving the {}x{} framebuffer, format {}",
        description.width,
        description.height,
        description.format
    );
    Ok((gpu, manager))
}
