//! The driver itself: it initializes the device through the virtio
//! library with its control and cursor queues, accepting EDID when the
//! device offers it, reads its scanouts, and publishes its GPU device
//! with the gpudev library, which lets one client at a time own the
//! display and shows the log on the first connected scanout while none
//! does. A display change that the device reports reads the scanouts
//! again and tells the owner. `Stop` resets the device, and `Reconnect`
//! replaces its bus channel.

mod display;
mod gpu;
mod log;

use crate::edid;
use crate::protocol::{self, CONFIG_EVENTS_CLEAR, CONFIG_EVENTS_READ, CONFIG_NUM_SCANOUTS};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::future::Future;
use display::{Display, Output};
use gpu::Gpu;
use log::LogScreen;
use mikros_gpudev::{Backend, Client, GpuDevice, Screen};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverReconnectResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, DeviceInfo, DisplayAsyncServer, Mode, ModeFlags, Render,
};
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use mikros_virtio::Transport;

const KIND_DMA: u32 = 4;
/// The refresh rate of a scanout's current size, which the device does
/// not tell.
const REFRESH_MILLIHERTZ: u32 = 60_000;
/// The PCI IDs of virtio-gpu devices.
const VENDOR: u32 = 0x1af4;
const DEVICE: u32 = 0x1050;

/// The GPU device as the gpudev library serves it.
struct Device {
    display: Rc<Display>,
    log: Rc<LogScreen>,
}

impl Backend for Device {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            name: "Virtio GPU".into(),
            driver: "virtiogpu".into(),
            driver_version: 1,
            vendor_id: VENDOR,
            device_id: DEVICE,
            capabilities: Capabilities::DISPLAY,
        }
    }

    fn serve(self: Rc<Self>, server: Rc<DisplayAsyncServer>) -> impl Future<Output = ()> + 'static {
        self.log.owned();
        display::serve(self.display.clone(), server)
    }

    fn closed(&self) {
        self.display.closed(self.log.scanout());
        self.log.released();
    }

    /// Never called: the library refuses `OpenRender` without `RENDER`.
    fn open_render(
        self: Rc<Self>,
        _render: ServerEnd<Render>,
        _client: Client,
    ) -> Result<(), Status> {
        Err(Status::NotSupported)
    }

    fn with_screen(&self, draw: &mut dyn FnMut(&dyn Screen)) {
        draw(&*self.log);
    }
}

pub fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
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
    let server = DriverAsyncServer::new(end, reactor.clone());
    let process = managed.header.process.raw();
    let directory = Rc::new(DirectoryAsyncClient::new(
        managed.header.directory,
        reactor.clone(),
    ));
    let mut running: Option<(Rc<GpuDevice<Device>>, DeviceManagerAsyncClient)> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    match start(request, process, &directory, &reactor, &spawner).await {
                        Ok(r) => {
                            running = Some(r);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    if let Some((gpu, _)) = &running {
                        gpu.backend().display.gpu.transport.stop(&reactor).await;
                    }
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                // A restarted PCI bus driver gives a new bus channel.
                DriverRequest::Reconnect(request, responder) => match &running {
                    Some((gpu, _)) => {
                        gpu.backend().display.gpu.transport.reconnect(request.bus);
                        responder.reply(s, Ok(DriverReconnectResponse {}))
                    }
                    None => responder.reply(s, Err(Status::BadState)),
                },
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

/// Initializes the device that `request` gives, with its `DMA` resource,
/// reads its scanouts, and publishes its GPU device. Fails with
/// `INVALID_ARGS` without a bus channel or a `DMA` resource, with the
/// status of the bus driver when it refused a request, and with
/// `INTERNAL` when the device could not be initialized.
async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    directory: &Rc<DirectoryAsyncClient>,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<(Rc<GpuDevice<Device>>, DeviceManagerAsyncClient), Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let dma = request
        .resources
        .into_iter()
        .find(|r| kind(r) == Some(KIND_DMA))
        .ok_or(Status::InvalidArgs)?;
    let device = DeviceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
    let started =
        Transport::start(device, process, &dma, protocol::F_EDID, 2, reactor, spawner).await;
    let started = started.map_err(|e| match e {
        mikros_virtio::Error::Bus(status) => status,
        _ => Status::Internal,
    })?;
    let edid = started.features & protocol::F_EDID != 0;
    let transport = started.transport.clone();
    let gpu = Rc::new(Gpu::new(transport, &started.queues, dma, process)?);
    let outputs = outputs(&gpu, edid).await?;
    let first = first_connected(&outputs);
    let display = Rc::new(Display::new(
        gpu.clone(),
        outputs,
        reactor.clone(),
        spawner.clone(),
    ));
    let log = Rc::new(LogScreen::new(gpu.clone()));
    if let Some((scanout, width, height)) = first {
        log.place(scanout, width, height).await;
    }
    spawner.spawn(log.clone().flush());
    let backend = Rc::new(Device {
        display: display.clone(),
        log: log.clone(),
    });
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let published = GpuDevice::publish(
        backend,
        &manager,
        directory.clone(),
        reactor.clone(),
        spawner.clone(),
    )
    .await?;
    spawner.spawn(watch_displays(display, log, edid));
    mikros_rt::log!(
        Info,
        "virtiogpu: serving {} scanouts, EDID {}",
        display_count(&published),
        if edid { "on" } else { "off" }
    );
    Ok((published, manager))
}

fn display_count(gpu: &GpuDevice<Device>) -> usize {
    gpu.backend().display.outputs.borrow().len()
}

/// The scanout, width and height of the first connected output.
fn first_connected(outputs: &[Output]) -> Option<(u32, u32, u32)> {
    outputs
        .iter()
        .find(|o| o.enabled)
        .and_then(|o| o.modes.first().map(|m| (o.index, m.width, m.height)))
}

/// The device's scanouts: each one's current size as its preferred mode,
/// and with `edid` its EDID and the modes that the EDID lists.
async fn outputs(gpu: &Gpu, edid: bool) -> Result<Vec<Output>, Status> {
    let count = gpu.transport.read_config(|c| c.read32(CONFIG_NUM_SCANOUTS)) as usize;
    let response = gpu
        .query(&protocol::get_display_info(), protocol::DISPLAY_INFO_SIZE)
        .await?;
    let scanouts = protocol::scanouts(&response, count).ok_or(Status::Io)?;
    let mut outputs = Vec::new();
    for (index, scanout) in scanouts.iter().enumerate() {
        let (width, height) = (scanout.rect.width, scanout.rect.height);
        let enabled = scanout.enabled && width > 0 && height > 0;
        let mut modes = Vec::new();
        if enabled {
            modes.push(Mode {
                width,
                height,
                refresh_millihertz: REFRESH_MILLIHERTZ,
                flags: ModeFlags::PREFERRED,
            });
        }
        let mut blob = Vec::new();
        if edid {
            let response = gpu
                .query(&protocol::get_edid(index as u32), protocol::EDID_SIZE)
                .await?;
            blob = protocol::edid(&response).unwrap_or(&[]).to_vec();
            for t in edid::modes(&blob) {
                let preferred = t.width == width && t.height == height;
                if !preferred && modes.len() < 64 {
                    modes.push(Mode {
                        width: t.width,
                        height: t.height,
                        refresh_millihertz: t.refresh_millihertz,
                        flags: ModeFlags(0),
                    });
                }
            }
        }
        outputs.push(Output {
            index: index as u32,
            enabled,
            modes,
            edid: blob,
        });
    }
    Ok(outputs)
}

/// Reads the scanouts again on every display change that the device
/// reports, clears the event, tells the display owner, and moves the log
/// to the first connected scanout.
async fn watch_displays(display: Rc<Display>, log: Rc<LogScreen>, edid: bool) {
    let transport = display.gpu.transport.clone();
    let mut seen = transport.config_changes();
    loop {
        seen = transport.config_changed(seen).await;
        let events = transport.read_config(|c| c.read32(CONFIG_EVENTS_READ));
        if events & protocol::EVENT_DISPLAY == 0 {
            continue;
        }
        let outputs = outputs(&display.gpu, edid).await;
        transport.read_config(|c| c.write32(CONFIG_EVENTS_CLEAR, protocol::EVENT_DISPLAY));
        let Ok(outputs) = outputs else {
            continue;
        };
        let first = first_connected(&outputs);
        // The CRTCs were counted at the start.
        let kept = display.outputs.borrow().len();
        let mut now = outputs;
        now.truncate(kept);
        *display.outputs.borrow_mut() = now;
        if let Some((scanout, width, height)) = first {
            log.place(scanout, width, height).await;
        }
        if let Some(owner) = display.owner.borrow().upgrade() {
            let _ = owner.server().send_on_hotplug();
        }
    }
}

/// The kind of `resource`, if it is one.
fn kind(resource: &OwnedHandle) -> Option<u32> {
    mikros_rt::hardware::resource(resource.raw())
        .ok()
        .map(|r| r.kind)
}
