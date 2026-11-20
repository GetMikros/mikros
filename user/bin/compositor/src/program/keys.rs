//! The keyboards of the seat, read for the keys that switch screens:
//! Ctrl, Alt and a function key whose keysym is that of F2 to F7, or
//! `XF86Switch_VT_2` to `XF86Switch_VT_7`, which xkb's PC layouts give
//! them while Ctrl and Alt are held, ask the session manager for the
//! screen of a text console (MIKSPEC-0066.R12). Screen 1 is the
//! desktop's own. Other input comes with window management.

use mikros_evdev::codes::{EV_KEY, KEY_A};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::ClientEnd;
use mikros_rt::interfaces::mikros::input::{Device, DeviceAsyncClient, DeviceEvent};
use mikros_rt::interfaces::mikros::session::SeatAsyncServer;
use std::rc::Rc;

/// The keyboard layout until `/system/config/keyboard.yaml` names one.
const LAYOUT: &str = "us";
/// The keysyms of F1 and of `XF86Switch_VT_1`; those of the next keys
/// follow them.
const F1: u32 = 0xFFBE;
const SWITCH_VT_1: u32 = 0x1008_FE01;
/// The desktop's screen, and the number of screens.
const DESKTOP: u8 = 1;
const SCREENS: u32 = 7;

/// The screen, 1 to 7, that `keysym` shows with Ctrl and Alt.
fn screen_key(keysym: u32) -> Option<u8> {
    [F1, SWITCH_VT_1]
        .into_iter()
        .find_map(|first| keysym.checked_sub(first).filter(|&i| i < SCREENS))
        .map(|i| i as u8 + 1)
}

/// Reads the key events of the input device `device`, the instance
/// `instance`, while it is open, and asks through `seat` for the screens
/// whose keys are pressed. A keyboard that is not `accepted` is not
/// acted on.
pub async fn read(
    reactor: Reactor,
    seat: Rc<SeatAsyncServer>,
    instance: String,
    device: ClientEnd<Device>,
    accepted: bool,
) {
    let device = DeviceAsyncClient::new(device, reactor);
    let Ok(info) = device.get_info().await else {
        return;
    };
    let keyboard = info.info.keys[usize::from(KEY_A / 8)] & (1 << (KEY_A % 8)) != 0;
    if !keyboard || !accepted {
        return;
    }
    let names = mikros_xkb::Names {
        layout: LAYOUT.into(),
        ..Default::default()
    };
    let Some(mut keymap) = mikros_xkb::Keymap::new(&names) else {
        mikros_rt::log!(Error, "compositor: the keymap cannot be compiled");
        return;
    };
    mikros_rt::log!(Info, "compositor: reading the keyboard {instance}");
    while let Ok(DeviceEvent::OnEvents(batch)) = device.next_event().await {
        for event in batch
            .events
            .iter()
            .filter(|e| e.r#type == EV_KEY && e.value != 2)
        {
            let pressed = event.value == 1;
            let switch = pressed && keymap.modifier("Control") && keymap.modifier("Mod1");
            if let Some(screen) =
                screen_key(keymap.keysym(event.code)).filter(|&screen| switch && screen != DESKTOP)
            {
                let _ = seat.server().send_on_switch_requested(screen);
            }
            keymap.update(event.code, pressed);
        }
    }
}
