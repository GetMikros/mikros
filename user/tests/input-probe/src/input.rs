//! The input devices: one reader each, revocation, LEDs, and the key
//! presses and mouse movement that the runner sends through QEMU.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::input::{
    Bus, Device, DeviceAsyncClient, DeviceEvent, Input, InputAsyncClient,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::sys;

use crate::{connect, failed, instances, now};

const INPUT: &str = "device.input";
/// The PS/2 keyboard and mouse, and the USB keyboard.
const DEVICES: usize = 3;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const KEY_A: u16 = 30;
const BTN_LEFT: u16 = 0x110;
const REL_X: u16 = 0;
const LED_CAPSL: u16 = 1;
const SIGNALED: u32 = 1 << 3;
/// How long the runner has to press the keys and move the mouse.
const EVENTS_LIMIT: i64 = 120_000_000_000;

/// What has arrived from one device.
#[derive(Default)]
struct Seen {
    key_a: bool,
    moved: bool,
    clicked: bool,
}

struct Opened {
    name: String,
    bus: Bus,
    keyboard: bool,
    device: Rc<DeviceAsyncClient>,
}

async fn open(
    input: &InputAsyncClient,
    reactor: &Reactor,
    revoke: Option<OwnedHandle>,
) -> Result<DeviceAsyncClient, Status> {
    let (client, server) = endpoints::<Device>().map_err(|_| Status::NoResources)?;
    match input.open(server, revoke).await {
        Ok(Ok(_)) => Ok(DeviceAsyncClient::new(client, reactor.clone())),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}

fn event() -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::event_create(0, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), 1 << 31, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// Checks each input device and waits for a press of A on every
/// keyboard and a move and left click of every mouse.
pub async fn works(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Option<()> {
    let Some(names) = instances(directory, reactor, INPUT, DEVICES).await else {
        return failed("finding the input services");
    };
    let mut opened = Vec::new();
    for instance in names {
        let channel = connect::<Input>(directory, INPUT, instance.clone()).await?;
        let input = InputAsyncClient::new(channel, reactor.clone());
        // A reader that is revoked makes way for the next.
        let revoke = event()?;
        let first = match open(&input, reactor, duplicate(&revoke)).await {
            Ok(device) => device,
            Err(_) => return failed("Open"),
        };
        if !matches!(
            open(&input, reactor, None).await,
            Err(Status::AlreadyExists)
        ) {
            return failed("refusing a second reader");
        }
        // SAFETY: the call takes no pointers.
        unsafe { sys::object_signal(revoke.raw(), 0, SIGNALED) };
        if first.next_event().await.is_ok() {
            return failed("closing the revoked reader");
        }
        let device = match open(&input, reactor, None).await {
            Ok(device) => device,
            Err(_) => return failed("opening after the revocation"),
        };
        let Ok(info) = device.get_info().await else {
            return failed("GetInfo");
        };
        let info = info.info;
        let keyboard = info.keys[usize::from(KEY_A / 8)] & (1 << (KEY_A % 8)) != 0;
        if keyboard {
            let lit = device.set_leds(1 << LED_CAPSL).await;
            let state = device.get_state().await;
            if !matches!(lit, Ok(Ok(_))) || !matches!(state, Ok(s) if s.leds == 1 << LED_CAPSL) {
                return failed("lighting Caps Lock");
            }
            let _ = device.set_leds(0).await;
        } else if !matches!(device.set_leds(1).await, Ok(Err(Status::NotSupported))) {
            return failed("refusing LEDs without LEDs");
        }
        if !matches!(device.rumble(1, 1, 1).await, Ok(Err(Status::NotSupported))) {
            return failed("refusing Rumble");
        }
        mikros_rt::log!(Info, "input-probe: opened {} on {:?}", info.name, info.bus);
        opened.push(Opened {
            name: info.name,
            bus: info.bus,
            keyboard,
            device: Rc::new(device),
        });
    }
    let seen: Rc<RefCell<Vec<Seen>>> = Rc::new(RefCell::new(
        opened.iter().map(|_| Seen::default()).collect(),
    ));
    for (at, o) in opened.iter().enumerate() {
        spawner.spawn(watch(o.device.clone(), seen.clone(), at, o.name.clone()));
    }
    mikros_rt::log!(Info, "input-probe: waiting for input");
    let deadline = now() + EVENTS_LIMIT;
    loop {
        let done = seen
            .borrow()
            .iter()
            .zip(&opened)
            .all(|(s, o)| match o.keyboard {
                true => s.key_a,
                false => s.moved && s.clicked,
            });
        if done {
            return Some(());
        }
        if now() >= deadline {
            for (s, o) in seen.borrow().iter().zip(&opened) {
                if !(s.key_a || s.moved) {
                    mikros_rt::log!(Error, "input-probe: nothing from {} on {:?}", o.name, o.bus);
                }
            }
            return failed("receiving input");
        }
        let _ = reactor.sleep_until(now() + 100_000_000).await;
    }
}

/// Records what device `at` reports.
async fn watch(
    device: Rc<DeviceAsyncClient>,
    seen: Rc<RefCell<Vec<Seen>>>,
    at: usize,
    name: String,
) {
    while let Ok(DeviceEvent::OnEvents(e)) = device.next_event().await {
        let mut seen = seen.borrow_mut();
        let s = &mut seen[at];
        for event in &e.events {
            match (event.r#type, event.code, event.value) {
                (EV_KEY, KEY_A, 1) if !s.key_a => {
                    s.key_a = true;
                    mikros_rt::log!(Info, "input-probe: A pressed on {name}");
                }
                (EV_KEY, BTN_LEFT, 1) if !s.clicked => {
                    s.clicked = true;
                    mikros_rt::log!(Info, "input-probe: left button on {name}");
                }
                (EV_REL, REL_X, v) if v != 0 && !s.moved => {
                    s.moved = true;
                    mikros_rt::log!(Info, "input-probe: moved on {name}");
                }
                _ => {}
            }
        }
    }
}
