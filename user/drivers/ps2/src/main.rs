//! The i8042 PS/2 driver.
//!
//! It drives the `acpi:i8042` device. `Start` takes the controller's
//! data and command ports from the device's `IOPORT` resources, the
//! lower one being the data port, or from one resource covering both,
//! and the interrupts of its first and second ports from its `IRQ`
//! resources, in that order. It sets the controller up, puts the
//! keyboard in scan code set 2, and publishes the keyboard as an input
//! device.

#![no_std]
#![no_main]

extern crate alloc;

mod hub;
mod ports;

use alloc::rc::Rc;
use alloc::vec::Vec;
use hub::{Hub, KeyboardLeds};
use mikros_evdev::codes::{EV_KEY, EV_LED, EV_SYN};
use mikros_inputdev::InputDevice;
use mikros_ps2::controller::Controller;
use mikros_ps2::scancode::Decoder;
use mikros_ps2::{KEYBOARD_LEDS, keyboard_set_2};
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverReconnectResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::input::{Bus, DeviceInfo};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};
use ports::IoPorts;

mikros_rt::raw_main!(main);

const KIND_IOPORT: u32 = 2;
const KIND_IRQ: u32 = 3;
const INFO_RESOURCE: u32 = 5;
const INFO_RESOURCE_SIZE: usize = 0x18;
/// The offset of the command port from the data port when one resource
/// covers both.
const COMMAND_OFFSET: u64 = 4;
const SIGNALED: u32 = 1 << 3;

/// What the driver holds while it runs: the device's resources, the
/// interrupt objects, and the controller with its devices.
struct Running {
    _resources: Vec<OwnedHandle>,
    _interrupts: Rc<Vec<OwnedHandle>>,
    _hub: Rc<Hub>,
    _keyboard: Option<Rc<InputDevice<KeyboardLeds>>>,
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
    let process = managed.header.process.raw();
    let mut running: Option<Running> = None;
    // The ACPI server's bus channel, which the driver does not use but
    // holds, and which `Reconnect` replaces.
    let mut bus: Option<OwnedHandle> = None;
    executor.block_on(async {
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                DriverRequest::Start(mut request, responder) => {
                    if running.is_some() {
                        let _ = responder.reply(s, Err(Status::BadState));
                        continue;
                    }
                    let channel = request.bus.take();
                    match start(request, process, &reactor, &spawner).await {
                        Ok(r) => {
                            running = Some(r);
                            bus = channel;
                            responder.reply(s, Ok(DriverStartResponse {}))
                        }
                        Err(status) => responder.reply(s, Err(status)),
                    }
                }
                DriverRequest::Stop(_, responder) => {
                    let _ = responder.reply(s, DriverStopResponse {});
                    return 0;
                }
                DriverRequest::Reconnect(request, responder) => match running {
                    Some(_) => {
                        bus = Some(request.bus);
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

/// The kind, base and size of `resource`.
fn info(resource: &OwnedHandle) -> Option<(u32, u64, u64)> {
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
    if status != mikros_rt::OK {
        return None;
    }
    let u64_at = |at: usize| u64::from_le_bytes(info[at..at + 8].try_into().unwrap());
    let kind = u32::from_le_bytes(info[0..4].try_into().unwrap());
    Some((kind, u64_at(0x08), u64_at(0x10)))
}

/// The data and command ports of the resources' `ports`, as (base,
/// size), after enabling them for `process`.
fn enable_ports(
    resources: &[OwnedHandle],
    ports: &[(usize, u64, u64)],
    process: sys::Handle,
) -> Result<(u16, u16), Status> {
    for &(at, base, size) in ports {
        // SAFETY: the call takes no pointers.
        let status =
            unsafe { sys::process_ioports_enable(process, resources[at].raw(), base, size) };
        if status != mikros_rt::OK {
            return Err(Status::from_raw(status as i32));
        }
    }
    let (data, command) = match ports {
        [(_, base, size)] if *size > COMMAND_OFFSET => (*base, base + COMMAND_OFFSET),
        [(_, a, _), (_, b, _), ..] => (*a.min(b), *a.max(b)),
        _ => return Err(Status::InvalidArgs),
    };
    match (u16::try_from(data), u16::try_from(command)) {
        (Ok(data), Ok(command)) => Ok((data, command)),
        _ => Err(Status::InvalidArgs),
    }
}

/// An interrupt object for `irq`, edge-triggered and active high.
fn interrupt(resource: &OwnedHandle, irq: u64) -> Result<OwnedHandle, Status> {
    let irq = u32::try_from(irq).map_err(|_| Status::InvalidArgs)?;
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::interrupt_create(resource.raw(), irq, 0, &mut out) };
    if status != mikros_rt::OK {
        return Err(Status::from_raw(status as i32));
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

/// What the keyboard reports.
fn keyboard_info() -> DeviceInfo {
    DeviceInfo {
        name: "PS/2 keyboard".into(),
        bus: Bus::Ps2,
        vendor: 0,
        product: 0,
        version: 0,
        event_types: 1 << EV_SYN | 1 << EV_KEY | 1 << EV_LED,
        keys: Decoder::keys(),
        relative_axes: 0,
        absolute_axes: Vec::new(),
        leds: KEYBOARD_LEDS,
    }
}

async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<Running, Status> {
    let resources = request.resources;
    let mut ports = Vec::new();
    let mut irqs = Vec::new();
    for (at, resource) in resources.iter().enumerate() {
        match info(resource) {
            Some((KIND_IOPORT, base, size)) => ports.push((at, base, size)),
            Some((KIND_IRQ, base, _)) => irqs.push((at, base)),
            _ => {}
        }
    }
    let (data, command) = enable_ports(&resources, &ports, process).inspect_err(|_| {
        mikros_rt::log!(Error, "ps2: the device has no usable I/O ports");
    })?;
    let mut controller = Controller::new(IoPorts { data, command });
    let mut present = controller.init(&mut |_, _| {}).map_err(|e| {
        mikros_rt::log!(Error, "ps2: setting up the controller failed: {e:?}");
        Status::Io
    })?;
    if present.first && keyboard_set_2(&mut controller, &mut |_, _| {}).is_err() {
        mikros_rt::log!(Error, "ps2: the keyboard refused scan code set 2");
        present.first = false;
    }
    // The mouse is not driven yet.
    present.second = false;
    let mut interrupts = Vec::new();
    for (wanted, &(at, irq)) in [present.first, present.second].iter().zip(&irqs) {
        if *wanted {
            interrupts.push(interrupt(&resources[at], irq)?);
        }
    }
    controller
        .enable_interrupts(present)
        .map_err(|_| Status::Io)?;
    let hub = Rc::new(Hub::new(controller));
    let manager = DeviceManagerAsyncClient::new(request.manager, reactor.clone());
    let mut keyboard = None;
    if present.first {
        let device = InputDevice::new(keyboard_info(), KeyboardLeds(hub.clone()), reactor.clone());
        let _ = hub.keyboard.set(Rc::downgrade(&device));
        device.publish(&manager, spawner).await?;
        mikros_rt::log!(Info, "ps2: published the keyboard");
        keyboard = Some(device);
    }
    let interrupts = Rc::new(interrupts);
    for at in 0..interrupts.len() {
        spawner.spawn(watch(hub.clone(), interrupts.clone(), at, reactor.clone()));
    }
    // Bytes that came before the interrupts were on.
    hub.drain();
    spawner.spawn(keep(manager));
    Ok(Running {
        _resources: resources,
        _interrupts: interrupts,
        _hub: hub,
        _keyboard: keyboard,
    })
}

/// Holds the device manager's client, since closing it would end the
/// driver's place with the device manager.
async fn keep(_manager: DeviceManagerAsyncClient) {
    core::future::pending::<()>().await
}

/// Reads what the devices send each time interrupt `at` fires.
async fn watch(hub: Rc<Hub>, interrupts: Rc<Vec<OwnedHandle>>, at: usize, reactor: Reactor) {
    let raw = interrupts[at].raw();
    while reactor.signals(raw, SIGNALED).await.is_ok() {
        hub.drain();
        // SAFETY: the call takes no pointers.
        unsafe { sys::interrupt_ack(raw) };
    }
}
