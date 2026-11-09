//! The USB mass storage driver, for Bulk-Only Transport interfaces with
//! SCSI commands.
//!
//! `Start` takes the interface's bulk IN and bulk OUT endpoints from its
//! `Interface` bus channel and sets up the Bulk-Only Transport on them.

#![no_std]
#![no_main]

extern crate alloc;

mod bot;
mod lock;

use alloc::rc::Rc;
use bot::Bot;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::usb::InterfaceAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const KIND_DMA: u32 = 4;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// Bulk endpoints, from `bmAttributes`.
const BULK: u8 = 2;
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
    let mut transport: Option<(Rc<Bot>, Rc<OwnedHandle>, DeviceManagerAsyncClient)> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(_, responder) if transport.is_some() => {
                    responder.reply(s, Err(Status::BadState))
                }
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &reactor).await {
                        Ok((bot, dma, _serial, manager)) => {
                            transport = Some((bot, dma, manager));
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ResetChild(_, responder) => {
                    responder.reply(s, Err(Status::NotSupported))
                }
                DriverRequest::ConnectChild(_, responder) => {
                    responder.reply(s, Err(Status::NotFound))
                }
            };
        }
        0
    })
}

/// The transport of the interface that `request` gives, with its `DMA`
/// resource, the interface's serial number and the device manager.
/// Fails with `INVALID_ARGS` without a bus channel, a `DMA` resource, or
/// a bulk IN and a bulk OUT endpoint, and with `PEER_CLOSED` when the
/// host process does not answer.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
) -> Result<
    (
        Rc<Bot>,
        Rc<OwnedHandle>,
        alloc::string::String,
        DeviceManagerAsyncClient,
    ),
    Status,
> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let interface = InterfaceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let info = interface
        .get_info()
        .await
        .map_err(|_| Status::PeerClosed)?
        .info;
    let bulk = |input: bool| {
        let mut endpoints = info.endpoints.iter();
        let found =
            endpoints.find(|e| e.attributes & 3 == BULK && (e.address & 0x80 != 0) == input);
        found.map(|e| e.address).ok_or(Status::InvalidArgs)
    };
    let (bulk_in, bulk_out) = (bulk(true)?, bulk(false)?);
    let bot = Bot::new(interface, info.number, bulk_in, bulk_out, &dma, process)?;
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    Ok((Rc::new(bot), Rc::new(dma), info.serial, manager))
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
