//! The boot manager's use of UEFI.

mod launch;
mod others;
mod pointers;
mod power;
mod screen;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bootmenu::bmp;
use bootmenu::canvas::Canvas;
use bootmenu::entries::Config;
use bootmenu::input::{self, Action, Event, Input};
use bootmenu::load::{self, ENTRIES_PATH, Loaded, ReadError};
use bootmenu::menu::{Item, Menu};
use bootmenu::pointer;
use bootmenu::session::{self, Exit, KeyStroke};
use bootmenu::text::Text;
use core::time::Duration;
use others::Detected;
use pointers::Pointers;
use screen::Screen;
use uefi::boot::{EventType, ScopedProtocol, TimerTrigger, Tpl};
use uefi::fs::FileSystem;
use uefi::proto::console::text::{InputEx, Key, ScanCode};
use uefi::proto::device_path::DevicePath;
use uefi::{CString16, Status, boot, system};
use uefi_raw::protocol::console::KeyToggleState;

/// Reads the file at `path` on `volume`. `describe` describes a failure
/// other than a missing file.
fn read_file(
    volume: Result<&mut FileSystem, String>,
    path: &str,
    describe: impl FnOnce(uefi::fs::Error) -> String,
) -> Result<Vec<u8>, ReadError> {
    let volume = volume.map_err(ReadError::Other)?;
    let name = CString16::try_from(path)
        .map_err(|_| ReadError::Other(String::from("the path is not valid")))?;
    volume.read(name.as_ref()).map_err(|err| match err {
        uefi::fs::Error::Io(err) if err.uefi_error.status() == Status::NOT_FOUND => {
            ReadError::NotFound
        }
        err => ReadError::Other(describe(err)),
    })
}

/// Reads the entries file from the volume the boot manager was loaded from.
fn load_config(volume: Result<&mut FileSystem, Status>) -> Loaded {
    let volume = volume.map_err(|status| format!("{status:?}"));
    load::load_config(read_file(volume, ENTRIES_PATH, |err| format!("{err}")))
}

/// Reads the BMP image at `path` on the boot volume and scales it to fit
/// the tile.
fn load_icon(volume: Option<&mut FileSystem>, path: &str) -> Result<bmp::Image, String> {
    let volume = volume.ok_or_else(|| String::from("the boot volume was not found"));
    load::icon(read_file(volume, path, |err| match err {
        uefi::fs::Error::Io(err) => format!("{:?}", err.uefi_error.status()),
        err => format!("{err}"),
    }))
}

/// Builds the menu for `config`, followed by the `detected` systems, with
/// the icons on `volume`.
fn build_menu(
    config: &Config,
    detected: &[Detected],
    mut volume: Option<&mut FileSystem>,
    config_notice: Option<String>,
) -> Menu {
    load::build_menu(
        config,
        detected,
        |path| load_icon(volume.as_deref_mut(), path),
        config_notice,
        power::firmware_ui_supported(),
    )
}

/// Translates a key from the console into a menu event.
fn translate(key: Key) -> Event {
    let key = match key {
        Key::Special(ScanCode::RIGHT) => input::Key::Right,
        Key::Special(ScanCode::DOWN) => input::Key::Down,
        Key::Special(ScanCode::LEFT) => input::Key::Left,
        Key::Special(ScanCode::UP) => input::Key::Up,
        Key::Printable(c) => input::Key::Char(char::from(c)),
        Key::Special(_) => input::Key::OtherSpecial,
    };
    input::translate(key)
}

/// The console's extended text input, read by `session::key_held`.
struct Keyboard(ScopedProtocol<InputEx>);

impl session::Keyboard for Keyboard {
    fn expose_key_state(&mut self) {
        let _ = self
            .0
            .set_state(KeyToggleState::VALID | KeyToggleState::EXPOSED);
    }

    fn read_key_stroke(&mut self) -> Option<KeyStroke> {
        let data = self.0.read_key().ok()??;
        let (scan_code, unicode_char) = match data.key {
            Key::Special(scan) => (scan.0, 0),
            Key::Printable(c) => (0, u16::from(c)),
        };
        Some(KeyStroke {
            scan_code,
            unicode_char,
            shift_state: data.key_state.key_shift_state.map_or(0, |s| s.bits()),
        })
    }
}

/// Creates an event that is signaled every second.
fn second_timer() -> Option<uefi::Event> {
    // SAFETY: a timer event without a notification function has no
    // callback that could run with invalid state.
    let event = unsafe { boot::create_event(EventType::TIMER, Tpl::CALLBACK, None, None) }.ok()?;
    boot::set_timer(&event, TimerTrigger::Periodic(Duration::from_secs(1))).ok()?;
    Some(event)
}

/// Waits for the next key press, pointer input or tick of `timer`, and
/// translates it into menu events.
fn next_events(timer: Option<&uefi::Event>, pointers: &mut Pointers) -> Vec<Event> {
    system::with_stdin(|input| {
        loop {
            if let Ok(key) = input.wait_for_key_event() {
                // SAFETY: the clone is only used for this wait, while the
                // timer event is still open.
                let timer = timer.map(|t| unsafe { t.unsafe_clone() });
                let has_timer = timer.is_some();
                let events: Vec<uefi::Event> = core::iter::once(key)
                    .chain(timer)
                    .chain(pointers.wait_events())
                    .collect();
                match boot::wait_for_event(&events) {
                    Ok(1) if has_timer => return vec![Event::Second],
                    Ok(0) | Err(_) => {}
                    Ok(_) => {
                        let events = pointers.read();
                        if events.is_empty() {
                            continue;
                        }
                        return events;
                    }
                }
            }
            match input.read_key() {
                Ok(Some(key)) => return vec![translate(key)],
                Ok(None) => continue,
                Err(_) => return vec![Event::OtherKey],
            }
        }
    })
}

/// Draws `menu`, and the pointer once it has moved, and shows the result.
fn redraw(menu: &Menu, input: &Input, text: &mut Text, canvas: &mut Canvas, screen: &mut Screen) {
    menu.draw(text, canvas);
    if let Some((x, y)) = input.pointer() {
        pointer::draw(canvas, x, y);
    }
    screen.present(canvas);
}

/// Shows `menu` on `screen` until an item is activated, and returns it. The
/// screen and the pointers are released when this returns.
fn show(menu: &mut Menu, mut screen: Screen, text: &mut Text, default: usize) -> Item {
    let (width, height) = screen.size();
    let mut canvas = Canvas::new(width, height);
    let mut input = Input::new(width as i32, height as i32, default);
    let mut pointers = Pointers::open(width, height);
    let mut timer = menu.countdown.as_ref().and_then(|_| second_timer());
    redraw(menu, &input, text, &mut canvas, &mut screen);
    let item = 'outer: loop {
        let mut changed = false;
        for event in next_events(timer.as_ref(), &mut pointers) {
            match input.handle(menu, event) {
                Action::None => {}
                Action::Redraw => changed = true,
                Action::Activate(item) => break 'outer item,
            }
        }
        if menu.countdown.is_none()
            && let Some(event) = timer.take()
        {
            let _ = boot::close_event(event);
        }
        if changed {
            redraw(menu, &input, text, &mut canvas, &mut screen);
        }
    };
    if let Some(event) = timer {
        let _ = boot::close_event(event);
    }
    item
}

/// The firmware services of `session::run`.
struct Firmware<'a> {
    text: &'a mut Text,
    volume: Option<Box<DevicePath>>,
    detected: &'a [Detected],
}

impl session::Firmware for Firmware<'_> {
    type Screen = Screen;

    fn key_held(&mut self) -> bool {
        let mut keyboard = pointers::open_on_console::<InputEx>().map(Keyboard);
        session::key_held(keyboard.as_mut())
    }

    fn open_screen(&mut self) -> Option<Screen> {
        Screen::open()
    }

    fn disable_watchdog(&mut self) {
        let _ = boot::set_watchdog_timer(0, 0x10000, None);
    }

    fn show(&mut self, menu: &mut Menu, screen: Screen, default: usize) -> Item {
        show(menu, screen, self.text, default)
    }

    fn start_file(&mut self, path: &str, args: Option<&str>) -> Result<(), String> {
        let volume = self
            .volume
            .as_deref()
            .ok_or_else(|| String::from("the boot volume was not found"))?;
        launch::start(volume, path, args)
    }

    fn start_detected(&mut self, index: usize) -> Result<(), String> {
        let system = &self.detected[index];
        launch::start_image(&system.device_path, &system.path, None)
    }
}

pub fn run() -> Status {
    let image_file_system = || {
        boot::get_image_file_system(boot::image_handle())
            .map(FileSystem::new)
            .map_err(|err| err.status())
    };
    // The boot volume is closed while other systems are detected. Detection
    // opens and closes the file system of every volume, and closing a
    // protocol drops every open of it by the same image, which would leave
    // this one closed underneath.
    let mut file_system = image_file_system();
    let mut loaded = load_config(file_system.as_mut().map_err(|s| *s));
    drop(file_system);
    let volume = launch::boot_volume();
    let detected = match &volume {
        Some(volume) if loaded.config.detect => others::find(&loaded.config, volume),
        _ => Vec::new(),
    };
    load::apply_detection(&mut loaded, detected.len());
    let Loaded { config, notice, .. } = loaded;
    let mut file_system = image_file_system();
    let menu = build_menu(&config, &detected, file_system.as_mut().ok(), notice);
    drop(file_system);
    let Some(mut text) = Text::new() else {
        return Status::LOAD_ERROR;
    };
    let mut firmware = Firmware {
        text: &mut text,
        volume,
        detected: &detected,
    };
    match session::run(&mut firmware, &config, menu) {
        Exit::Power(power) => power::act(power),
        Exit::NoScreen(Ok(())) => Status::SUCCESS,
        Exit::NoScreen(Err(_)) => Status::LOAD_ERROR,
    }
}
