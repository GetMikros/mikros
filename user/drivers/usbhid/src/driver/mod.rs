//! The USB HID driver.
//!
//! It drives `usb:c03` interfaces. `Start` reads the report descriptor,
//! whose length the interface's HID descriptor gives, selects the report
//! protocol on boot interfaces, sends SET_IDLE with duration 0 to
//! keyboards, ignoring a stall, and publishes an input device for each
//! keyboard, mouse and pointer application collection. It then keeps
//! interrupt IN transfers queued on the interface's input endpoint and
//! turns each report into events for the devices. `SetLeds` sends the
//! LED output report on the interrupt OUT endpoint when there is one,
//! and with SET_REPORT otherwise. `Reconnect` gives a new interface
//! channel once the host controller's driver restarted, on which the
//! transfers start again.

mod usb;
use crate::hid::descriptor::{self, Kind};
use crate::hid::mapping::{self, Application, Device};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_evdev::{Event, group};
use mikros_inputdev::{Backend, InputDevice};
use mikros_rt::exec::{Executor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverReconnectResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::input::{AbsInfo, Bus, DeviceInfo};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::usb::{Interface, InterfaceAsyncClient, InterfaceInfo};
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The boot interface subclass.
const BOOT: u8 = 1;
/// Interrupt endpoints, from `bmAttributes`, and the IN direction bit
/// of an endpoint address.
const INTERRUPT: u8 = 3;
const IN: u8 = 0x80;
/// The most an interrupt transfer carries.
const TRANSFER_MAX: usize = 1024;

/// The interface while the driver runs.
struct Hid {
    bus: RefCell<Rc<InterfaceAsyncClient>>,
    /// Which channel the polling task reads; a new one stops the old.
    generation: Cell<u64>,
    number: u8,
    input: u8,
    output: Option<u8>,
    input_len: u32,
    report_ids: bool,
    devices: RefCell<Vec<Published>>,
}

/// The device of one application, and its published input device.
type Published = (Rc<RefCell<Device>>, Rc<InputDevice<Leds>>);

/// Sets a keyboard's LEDs.
struct Leds {
    hid: Rc<Hid>,
    device: Rc<RefCell<Device>>,
}

impl Backend for Leds {
    async fn set_leds(&self, leds: u16) -> Result<(), Status> {
        let report = self
            .device
            .borrow()
            .led_report(leds)
            .ok_or(Status::NotSupported)?;
        let bus = self.hid.bus.borrow().clone();
        match self.hid.output {
            Some(endpoint) => crate::driver::usb::interrupt_out(&bus, endpoint, report).await,
            None => {
                let id = if self.hid.report_ids { report[0] } else { 0 };
                crate::driver::usb::set_report(&bus, self.hid.number, id, report).await
            }
        }
    }
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
    let Some(at) = managed
        .handles
        .iter()
        .position(|h| h.role == HandleRole::Provider)
    else {
        return PANIC_EXIT_CODE;
    };
    let end = ServerEnd::new(managed.handles.remove(at).handle);
    let server = DriverAsyncServer::new(end, reactor.clone());
    let mut running: Option<(Rc<Hid>, DeviceManagerAsyncClient)> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(request, responder) => {
                    if running.is_some() {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    }
                    match start(request, &reactor, &spawner).await {
                        Ok(r) => {
                            running = Some(r);
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(request, responder) => match &running {
                    Some((hid, _)) => {
                        let bus = ClientEnd::<Interface>::new(request.bus);
                        let bus = Rc::new(InterfaceAsyncClient::new(bus, reactor.clone()));
                        *hid.bus.borrow_mut() = bus;
                        spawner.spawn(read(hid.clone()));
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

/// The interrupt endpoint of the interface in the direction of `input`.
fn endpoint(info: &InterfaceInfo, input: bool) -> Option<u8> {
    info.endpoints
        .iter()
        .find(|e| e.attributes & 0b11 == INTERRUPT && (e.address & IN != 0) == input)
        .map(|e| e.address)
}

/// What the device of one application reports.
fn device_info(info: &InterfaceInfo, application: Application, device: &Device) -> DeviceInfo {
    let c = device.capabilities();
    let kind = match application {
        Application::Keyboard => "keyboard",
        Application::Mouse => "mouse",
        Application::Pointer => "pointer",
    };
    let mut name: String = match info.product.is_empty() {
        true => format!("USB {kind}"),
        false => format!("{} {kind}", info.product),
    };
    name.truncate(name.floor_char_boundary(128));
    DeviceInfo {
        name,
        bus: Bus::Usb,
        vendor: info.vendor_id,
        product: info.product_id,
        version: info.device_version,
        event_types: c.event_types,
        keys: c.keys,
        relative_axes: c.relative_axes,
        absolute_axes: c
            .absolute_axes
            .iter()
            .map(|a| AbsInfo {
                code: a.code,
                minimum: a.minimum,
                maximum: a.maximum,
                resolution: 0,
            })
            .collect(),
        leds: c.leds,
    }
}

async fn start(
    request: DriverStartRequest,
    reactor: &mikros_rt::exec::Reactor,
    spawner: &Spawner,
) -> Result<(Rc<Hid>, DeviceManagerAsyncClient), Status> {
    let bus = request.bus.ok_or(Status::InvalidArgs)?;
    let bus = Rc::new(InterfaceAsyncClient::new(
        ClientEnd::new(bus),
        reactor.clone(),
    ));
    let info = match bus.get_info().await {
        Ok(reply) => reply.info,
        Err(_) => return Err(Status::PeerClosed),
    };
    let configuration = match bus.get_configuration_descriptor().await {
        Ok(reply) => reply.data,
        Err(_) => return Err(Status::PeerClosed),
    };
    let number = info.number;
    let len =
        crate::driver::usb::report_descriptor_len(&configuration, number).ok_or_else(|| {
            mikros_rt::log!(Error, "usbhid: interface {number} has no HID descriptor");
            Status::NotSupported
        })?;
    let input = endpoint(&info, true).ok_or(Status::NotSupported)?;
    let bytes = crate::driver::usb::report_descriptor(&bus, number, len).await?;
    if info.subclass == BOOT {
        crate::driver::usb::set_report_protocol(&bus, number).await?;
    }
    let parsed = descriptor::parse(&bytes).map_err(|e| {
        mikros_rt::log!(Error, "usbhid: the report descriptor is not valid: {e:?}");
        Status::NotSupported
    })?;
    let applications = mapping::applications(&parsed);
    if applications
        .iter()
        .any(|&(_, a)| a == Application::Keyboard)
    {
        // A device that does not support idle rates stalls the request.
        let _ = crate::driver::usb::set_idle(&bus, number).await;
    }
    let hid = Rc::new(Hid {
        bus: RefCell::new(bus),
        generation: Cell::new(0),
        number,
        input,
        output: endpoint(&info, false),
        input_len: parsed.report_len(Kind::Input).min(TRANSFER_MAX) as u32,
        report_ids: parsed.report_ids,
        devices: RefCell::new(Vec::new()),
    });
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    for (at, application) in applications {
        let device = Rc::new(RefCell::new(Device::new(&parsed, at)));
        let device_info = device_info(&info, application, &device.borrow());
        let leds = Leds {
            hid: hid.clone(),
            device: device.clone(),
        };
        let input = InputDevice::new(device_info, leds, reactor.clone());
        input.publish(&manager, spawner).await?;
        mikros_rt::log!(
            Info,
            "usbhid: published a {application:?} on interface {number}"
        );
        hid.devices.borrow_mut().push((device, input));
    }
    spawner.spawn(read(hid.clone()));
    Ok((hid, manager))
}

/// Reads the input reports of the current channel and reports their
/// events, until the transfers fail or a new channel replaces it.
async fn read(hid: Rc<Hid>) {
    let generation = hid.generation.get() + 1;
    hid.generation.set(generation);
    let bus = hid.bus.borrow().clone();
    let reader = hid.clone();
    let status = crate::driver::usb::poll(bus, hid.input, hid.input_len, move |report| {
        if reader.generation.get() != generation {
            return;
        }
        let timestamp = crate::driver::usb::now();
        for (device, input) in reader.devices.borrow().iter() {
            let events = device.borrow_mut().decode(&report);
            if !events.is_empty() {
                let events = events
                    .into_iter()
                    .map(|(kind, code, value)| Event::new(timestamp, kind, code, value));
                input.report(&group(timestamp, events));
            }
        }
    })
    .await;
    if hid.generation.get() == generation {
        mikros_rt::log!(
            Warning,
            "usbhid: reading interface {} stopped: {status:?}",
            hid.number
        );
    }
}
