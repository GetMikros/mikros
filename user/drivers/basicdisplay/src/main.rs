//! The basic display driver.
//!
//! It is the fallback driver of the `platform:boot-framebuffer` device.
//! It takes the framebuffer's memory object from its startup handle with
//! the role `USER` and ID 1, and its description from the device's
//! properties, and publishes the service class `gpu`. It serves the
//! `Display` part of `mikros.gpu` with one connector, CRTC and primary
//! plane in the framebuffer's mode, dumb buffers in XRGB8888, and
//! commits that it presents by copying the plane's buffer into the
//! framebuffer, converting each pixel to the framebuffer's format.
//! One client at a time, holding `gpu.display`, owns the display.

#![no_std]
#![no_main]

extern crate alloc;

mod display;
mod framebuffer;

use alloc::rc::Rc;
use core::cell::RefCell;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use display::Display;
use framebuffer::{Description, Framebuffer};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverRequest, DriverStartRequest,
    DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, Device, DeviceAsyncServer, DeviceGetInfoResponse, DeviceInfo,
    DeviceOpenDisplayResponse, DeviceRequest, DisplayAsyncServer,
};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerAsyncClient, Decision};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, Provider, ProviderAsyncServer, ProviderRequest,
};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the framebuffer's startup handle among the `USER` ones.
const FRAMEBUFFER_ID: u32 = 1;
const CLASS: &str = "gpu";
const PROTOCOL: &str = "device.gpu";
const BROKER: &str = "mikros.permissions.Broker";
const GPU_DISPLAY: &str = "gpu.display";
const SIGNALED: u32 = 1 << 3;

/// What the connections share.
struct Gpu {
    display: Rc<Display>,
    broker: Option<BrokerAsyncClient>,
    owner: RefCell<Option<Rc<DisplayAsyncServer>>>,
    reactor: Reactor,
    spawner: Spawner,
}

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
    let directory = DirectoryAsyncClient::new(managed.header.directory, reactor.clone());
    let mut running: Option<(Rc<Gpu>, DeviceManagerAsyncClient)> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
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
                // presented once the framebuffer is unmapped.
                DriverRequest::Stop(_, responder) => {
                    if let Some((gpu, _)) = &running
                        && let Some(fb) = gpu.display.framebuffer.borrow_mut().take()
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
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Rc<Gpu>, DeviceManagerAsyncClient), Status> {
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
    let gpu = Rc::new(Gpu {
        display,
        broker: broker(directory, reactor).await,
        owner: RefCell::new(None),
        reactor: reactor.clone(),
        spawner: spawner.clone(),
    });
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let (client, server) = endpoints::<Provider>().map_err(|_| Status::NoResources)?;
    match manager.publish_service(CLASS.into(), client).await {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::PeerClosed),
    }
    spawner.spawn(serve_provider(gpu.clone(), server));
    mikros_rt::log!(
        Info,
        "basicdisplay: serving the {}x{} framebuffer, format {}",
        description.width,
        description.height,
        description.format
    );
    Ok((gpu, manager))
}

/// The permission broker, through `directory`: None when it cannot be
/// reached, so that no client gets the display.
async fn broker(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<BrokerAsyncClient> {
    let (client, server) = endpoints::<Broker>().ok()?;
    match directory
        .connect(BROKER.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => Some(BrokerAsyncClient::new(client, reactor.clone())),
        _ => None,
    }
}

async fn serve_provider(gpu: Rc<Gpu>, end: ServerEnd<Provider>) {
    let provider = ProviderAsyncServer::new(end, gpu.reactor.clone());
    while let Ok(ProviderRequest::Connect(c)) = provider.next().await {
        // Dropping the server end of another protocol closes it.
        if c.protocol == PROTOCOL {
            let end = ServerEnd::<Device>::new(c.server);
            gpu.spawner.spawn(serve_device(gpu.clone(), end, c.client));
        }
    }
}

/// Whether `client` holds `gpu.display`, as the broker says.
async fn may_own(gpu: &Gpu, client: &ClientIdentity) -> bool {
    let Some(broker) = &gpu.broker else {
        return false;
    };
    let client = ClientIdentity {
        pid: client.pid,
        user: client.user,
        app_id: client.app_id.clone(),
    };
    let checked = broker.check(client, GPU_DISPLAY.into()).await;
    matches!(checked, Ok(Ok(r)) if r.decision == Decision::Allowed)
}

async fn serve_device(gpu: Rc<Gpu>, end: ServerEnd<Device>, client: ClientIdentity) {
    let server = DeviceAsyncServer::new(end, gpu.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            DeviceRequest::GetInfo(_, r) => r.reply(
                s,
                DeviceGetInfoResponse {
                    info: DeviceInfo {
                        name: "Basic Display".into(),
                        driver: "basicdisplay".into(),
                        driver_version: 1,
                        vendor_id: 0,
                        device_id: 0,
                        capabilities: Capabilities::DISPLAY,
                    },
                },
            ),
            DeviceRequest::OpenDisplay(q, r) => {
                let result = if !may_own(&gpu, &client).await {
                    Err(Status::AccessDenied)
                } else if gpu.owner.borrow().is_some() {
                    Err(Status::AlreadyExists)
                } else {
                    // The owner's place is taken before the connection is
                    // first served, so that the next `OpenDisplay` fails.
                    let owner = Rc::new(DisplayAsyncServer::new(q.display, gpu.reactor.clone()));
                    *gpu.owner.borrow_mut() = Some(owner.clone());
                    gpu.spawner.spawn(own(gpu.clone(), owner, q.revoke));
                    Ok(DeviceOpenDisplayResponse {})
                };
                r.reply(s, result)
            }
            DeviceRequest::OpenRender(_, r) => r.reply(s, Err(Status::NotSupported)),
        };
    }
}

/// Serves the display owner until it closes its connection or `revoke`
/// is signaled, then frees what the connection had.
async fn own(gpu: Rc<Gpu>, owner: Rc<DisplayAsyncServer>, revoke: Option<OwnedHandle>) {
    let serving = display::serve(gpu.display.clone(), owner.clone());
    match &revoke {
        Some(event) => first(serving, gpu.reactor.signals(event.raw(), SIGNALED)).await,
        None => serving.await,
    }
    *gpu.owner.borrow_mut() = None;
    gpu.display.closed();
}

/// Completes when the first of `a` and `b` completes.
async fn first<A: Future, B: Future>(a: A, b: B) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if a.as_mut().poll(cx).is_ready() || b.as_mut().poll(cx).is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}
