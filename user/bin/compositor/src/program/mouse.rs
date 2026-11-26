//! The input devices of the seat: keyboards, whose keys `keys` reads, and
//! pointer devices: mice, which move the pointer by
//! relative motion, and tablets and touchpads with absolute axes, whose
//! range covers the output. Each report of a device moves the pointer,
//! then presses or releases its buttons and turns its wheels, and ends
//! with a frame (MIKSPEC-0066.R16); the pointer is drawn again where it
//! moved (MIKSPEC-0066.R28).

use super::Compositor;
use super::keys::Keys;
use mikros_evdev::codes::{
    ABS_X, ABS_Y, BTN_LEFT, EV_ABS, EV_KEY, EV_REL, EV_SYN, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y,
    SYN_REPORT,
};
use mikros_rt::interfaces::__midl::ClientEnd;
use mikros_rt::interfaces::mikros::input::{AbsInfo, Device, DeviceAsyncClient, DeviceEvent};
use mikros_rt::interfaces::mikros::session::SeatAsyncServer;
use std::rc::Rc;

/// The buttons of a pointer: the evdev codes from BTN_LEFT on.
const BUTTONS: std::ops::Range<u16> = BTN_LEFT..BTN_LEFT + 8;

/// What a report of a device changes.
#[derive(Default)]
struct Report {
    dx: i32,
    dy: i32,
    /// The absolute position on the output, when the device gave one.
    x: Option<f64>,
    y: Option<f64>,
    wheel: i32,
    hwheel: i32,
    buttons: Vec<(u32, bool)>,
}

/// Where `value` of the axis `info` lies on `length` pixels.
fn scale(info: &AbsInfo, value: i32, length: i32) -> f64 {
    let range = f64::from(info.maximum) - f64::from(info.minimum);
    if range <= 0.0 {
        return 0.0;
    }
    (f64::from(value) - f64::from(info.minimum)) / range * f64::from(length - 1)
}

impl Compositor {
    /// Reads the events of the input device `device`, the instance
    /// `instance`, while it is open: its keys as a keyboard's, which ask
    /// through `seat` for screens, when it has letter keys, and its motion,
    /// buttons and wheels as a pointer's when it has relative or absolute
    /// X and Y axes. A device that is not `accepted` is not read.
    pub(super) async fn read_input(
        self: Rc<Self>,
        seat: Rc<SeatAsyncServer>,
        instance: String,
        device: ClientEnd<Device>,
        accepted: bool,
    ) {
        let device = DeviceAsyncClient::new(device, self.reactor.clone());
        let Ok(info) = device.get_info().await else {
            return;
        };
        let info = info.info;
        if !accepted {
            return;
        }
        let mut keys = Keys::new(&self, &info, &instance, seat);
        let relative =
            info.relative_axes & (1 << REL_X) != 0 && info.relative_axes & (1 << REL_Y) != 0;
        let axis = |code| info.absolute_axes.iter().find(|a| a.code == code).cloned();
        let absolute = axis(ABS_X).zip(axis(ABS_Y));
        let pointer = relative || absolute.is_some();
        if pointer {
            mikros_rt::log!(Info, "compositor: reading the pointer {instance}");
        } else if keys.is_none() {
            return;
        }
        let mut report = Report::default();
        let mut time = 0;
        // The buttons that this device holds, to let go of when it goes.
        let mut held: Vec<u32> = Vec::new();
        while let Ok(DeviceEvent::OnEvents(batch)) = device.next_event().await {
            let mut keyed = false;
            for event in &batch.events {
                time = (event.timestamp / 1_000_000) as u32;
                match (event.r#type, event.code) {
                    (EV_REL, REL_X) => report.dx += event.value,
                    (EV_REL, REL_Y) => report.dy += event.value,
                    (EV_REL, REL_WHEEL) => report.wheel += event.value,
                    (EV_REL, REL_HWHEEL) => report.hwheel += event.value,
                    (EV_ABS, ABS_X | ABS_Y) => {
                        let Some((x_info, y_info)) = &absolute else {
                            continue;
                        };
                        let (width, height) = self.server.borrow().screen.unwrap_or((1, 1));
                        match event.code {
                            ABS_X => report.x = Some(scale(x_info, event.value, width)),
                            _ => report.y = Some(scale(y_info, event.value, height)),
                        }
                    }
                    (EV_KEY, code) if BUTTONS.contains(&code) => {
                        if pointer && event.value != 2 {
                            let (button, pressed) = (u32::from(code), event.value == 1);
                            report.buttons.push((button, pressed));
                            held.retain(|&b| b != button);
                            if pressed {
                                held.push(button);
                            }
                        }
                    }
                    (EV_KEY, code) => {
                        if let Some(keys) = &mut keys {
                            keys.key(&self, code, event.value, time);
                            keyed = true;
                        }
                    }
                    (EV_SYN, SYN_REPORT) if pointer => {
                        self.apply(std::mem::take(&mut report), time);
                    }
                    _ => {}
                }
            }
            if keyed {
                self.flush_all();
            }
        }
        if let Some(keys) = &mut keys {
            keys.release_all(&self, time);
            self.flush_all();
        }
        // A pointer that is gone ends the resize that one of its buttons
        // holds (MIKSPEC-0074.R30).
        if !held.is_empty() {
            self.server.borrow_mut().pointer_gone(&held);
            self.flush_all();
            self.changed.notify();
        }
    }

    /// Moves the pointer, presses its buttons and turns its wheels as
    /// `report` says, at `time` in milliseconds.
    fn apply(&self, report: Report, time: u32) {
        {
            let mut server = self.server.borrow_mut();
            let (x, y) = (server.pointer.x, server.pointer.y);
            let moved =
                report.dx != 0 || report.dy != 0 || report.x.is_some() || report.y.is_some();
            if moved {
                let x = report.x.unwrap_or(x + f64::from(report.dx));
                let y = report.y.unwrap_or(y + f64::from(report.dy));
                server.pointer_motion(x, y, time);
            }
            for (button, pressed) in report.buttons {
                server.pointer_button(button, pressed, time);
            }
            // A wheel's notches up and left are positive; scrolling down
            // and right is.
            if report.wheel != 0 || report.hwheel != 0 {
                server.pointer_axis(-report.wheel, report.hwheel, time);
            }
            server.pointer_frame();
        }
        self.flush_all();
        self.changed.notify();
    }
}
