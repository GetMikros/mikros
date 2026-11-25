//! The i8042 PS/2 driver.
//!
//! It drives the `acpi:i8042` device. `Start` takes the controller's
//! data and command ports from the device's `IOPORT` resources, the
//! lower one being the data port, or from one resource covering both,
//! and the interrupts of its first and second ports from its `IRQ`
//! resources, in that order. It sets the controller up, puts the
//! keyboard in scan code set 2, chooses the mouse's protocol, and
//! publishes the keyboard and the mouse as input devices.

mod hub;
mod ports;
use crate::driver::hub::{Hub, KeyboardLeds, NoLeds};
use crate::driver::ports::ControllerPorts;
use crate::ps2::controller::Controller;
use crate::ps2::mouse::{self, Protocol};
use crate::ps2::scancode::Decoder;
use crate::ps2::{KEYBOARD_LEDS, keyboard_set_2};
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_evdev::codes::{EV_KEY, EV_LED, EV_REL, EV_SYN, REL_WHEEL, REL_X, REL_Y};
use mikros_evdev::{KEY_BYTES, set_bit};
use mikros_inputdev::InputDevice;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::hardware::IoPorts;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{
    DeviceManagerAsyncClient, DriverAsyncServer, DriverReconnectResponse, DriverRequest,
    DriverStartRequest, DriverStartResponse, DriverStopResponse,
};
use mikros_rt::interfaces::mikros::input::{Bus, DeviceInfo};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::objects::now;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const KIND_IOPORT: u32 = 2;
const KIND_IRQ: u32 = 3;
/// The offset of the command port from the data port when one resource
/// covers both.
const COMMAND_OFFSET: u16 = 4;
const SIGNALED: u32 = 1 << 3;

/// What the driver holds while it runs: the device's resources but its
/// `IOPORT` ones, which the controller's ports hold, the interrupt
/// objects, and the controller with its devices.
struct Running {
    _resources: Vec<OwnedHandle>,
    _interrupts: Rc<Vec<OwnedHandle>>,
    _hub: Rc<Hub>,
    _keyboard: Option<Rc<InputDevice<KeyboardLeds>>>,
    _mouse: Option<Rc<InputDevice<NoLeds>>>,
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

/// The data and command ports of `ports`, the device's `IOPORT`
/// resources as (resource, base, size), after enabling them for
/// `process`.
fn enable_ports(
    ports: Vec<(OwnedHandle, u64, u64)>,
    process: sys::Handle,
) -> Result<ControllerPorts, Status> {
    let mut enabled = Vec::new();
    let mut bases = Vec::new();
    for (resource, base, size) in ports {
        let (Ok(base), Ok(count)) = (u16::try_from(base), u16::try_from(size)) else {
            return Err(Status::InvalidArgs);
        };
        let ports = IoPorts::enable(process, resource, base, count)
            .map_err(|s| Status::from_raw(s as i32))?;
        enabled.push(ports);
        bases.push((base, count));
    }
    let (data, command) = match bases[..] {
        [(_, count)] if count > COMMAND_OFFSET => ((0, 0), (0, COMMAND_OFFSET)),
        [(a, _), (b, _), ..] if a <= b => ((0, 0), (1, 0)),
        [_, _, ..] => ((1, 0), (0, 0)),
        _ => return Err(Status::InvalidArgs),
    };
    Ok(ControllerPorts {
        enabled,
        data,
        command,
    })
}

/// An interrupt object for `irq`, edge-triggered and active high.
fn interrupt(resource: &OwnedHandle, irq: u64) -> Result<OwnedHandle, Status> {
    let irq = u32::try_from(irq).map_err(|_| Status::InvalidArgs)?;
    mikros_rt::hardware::interrupt(resource.raw(), irq, 0).map_err(|s| Status::from_raw(s as i32))
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

/// What a mouse in `protocol` reports.
fn mouse_info(protocol: Protocol) -> DeviceInfo {
    let mut keys = [0; KEY_BYTES];
    for &button in protocol.buttons() {
        set_bit(&mut keys, button);
    }
    let mut relative_axes = 1 << REL_X | 1 << REL_Y;
    if protocol.has_wheel() {
        relative_axes |= 1 << REL_WHEEL;
    }
    DeviceInfo {
        name: "PS/2 mouse".into(),
        bus: Bus::Ps2,
        vendor: 0,
        product: 0,
        version: 0,
        event_types: 1 << EV_SYN | 1 << EV_KEY | 1 << EV_REL,
        keys,
        relative_axes,
        absolute_axes: Vec::new(),
        leds: 0,
    }
}

async fn start(
    request: DriverStartRequest,
    process: sys::Handle,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<Running, Status> {
    let mut resources = Vec::new();
    let mut ports = Vec::new();
    let mut irqs = Vec::new();
    for resource in request.resources {
        match mikros_rt::hardware::resource(resource.raw()) {
            Ok(r) if r.kind == KIND_IOPORT => ports.push((resource, r.base, r.size)),
            Ok(r) if r.kind == KIND_IRQ => {
                irqs.push((resources.len(), r.base));
                resources.push(resource);
            }
            _ => resources.push(resource),
        }
    }
    let ports = enable_ports(ports, process).inspect_err(|_| {
        mikros_rt::log!(Error, "ps2: the device has no usable I/O ports");
    })?;
    let mut controller = Controller::new(ports);
    let mut present = controller.init(&mut |_, _| {}).map_err(|e| {
        mikros_rt::log!(Error, "ps2: setting up the controller failed: {e:?}");
        Status::Io
    })?;
    if present.first && keyboard_set_2(&mut controller, &mut |_, _| {}).is_err() {
        mikros_rt::log!(Error, "ps2: the keyboard refused scan code set 2");
        present.first = false;
    }
    let mut protocol = Protocol::Standard;
    if present.second {
        match mouse::setup(&mut controller, &mut |_, _| {}) {
            Ok(p) => protocol = p,
            Err(e) => {
                mikros_rt::log!(Error, "ps2: setting up the mouse failed: {e:?}");
                present.second = false;
            }
        }
    }
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
    let mut mouse = None;
    if present.second {
        let device = InputDevice::new(mouse_info(protocol), NoLeds, reactor.clone());
        let decoder = RefCell::new(mouse::Decoder::new(protocol));
        let _ = hub.mouse.set((decoder, Rc::downgrade(&device)));
        device.publish(&manager, spawner).await?;
        mikros_rt::log!(
            Info,
            "ps2: published the mouse, protocol {}",
            protocol as u8
        );
        mouse = Some(device);
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
        _mouse: mouse,
    })
}

/// Holds the device manager's client, since closing it would end the
/// driver's place with the device manager.
async fn keep(_manager: DeviceManagerAsyncClient) {
    core::future::pending::<()>().await
}

/// How many times in a row the controller's buffer is read again before
/// the driver lets the other tasks run for `REDRAIN_PAUSE` nanoseconds.
const REDRAIN_ROUNDS: u32 = 64;
const REDRAIN_PAUSE: i64 = 1_000_000;

/// Reads what the devices send each time interrupt `at` fires.
async fn watch(hub: Rc<Hub>, interrupts: Rc<Vec<OwnedHandle>>, at: usize, reactor: Reactor) {
    let raw = interrupts[at].raw();
    while reactor.signals(raw, SIGNALED).await.is_ok() {
        // The line is masked from one interrupt until it is acknowledged,
        // and an edge that comes while it is masked is lost. A byte that
        // arrived after the last read leaves the controller's output
        // buffer full, and it takes in no other byte until that is read,
        // so the buffer is looked at again once the line is open, and
        // read, until it is empty with the line open.
        let mut rounds = 0u32;
        loop {
            hub.drain();
            let _ = mikros_rt::hardware::interrupt_ack(raw);
            if !hub.pending() {
                break;
            }
            // A controller that always has a byte, and a line that opens
            // and fills again, must not keep the other tasks from running.
            rounds += 1;
            if rounds.is_multiple_of(REDRAIN_ROUNDS) {
                let _ = reactor.sleep_until(now() + REDRAIN_PAUSE).await;
            }
        }
    }
}
