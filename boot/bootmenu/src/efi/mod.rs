//! The boot manager's use of UEFI.

mod launch;
mod pointers;
mod power;
mod screen;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bootmenu::bmp;
use bootmenu::canvas::{Canvas, Color};
use bootmenu::entries::Config;
use bootmenu::input::{Action, Event, Input};
use bootmenu::menu::{Card, FocusSource, ICON_SIZE, Item, Menu, Power};
use bootmenu::pointer;
use bootmenu::text::Text;
use core::time::Duration;
use pointers::Pointers;
use screen::Screen;
use uefi::boot::{EventType, TimerTrigger, Tpl};
use uefi::fs::FileSystem;
use uefi::proto::console::text::{InputEx, Key, ScanCode};
use uefi::proto::device_path::DevicePath;
use uefi::runtime::{self, VariableVendor};
use uefi::{CString16, Status, boot, cstr16, system};
use uefi_raw::protocol::console::KeyToggleState;

const ENTRIES_PATH: &str = "\\EFI\\Mikros\\entries.yaml";

/// Bit of `OsIndicationsSupported` and `OsIndications` that asks the
/// firmware to show its setup screen on the next boot.
const BOOT_TO_FW_UI: u64 = 1;

/// Reads the entries file from the volume the boot manager was loaded from.
/// Returns the configuration and a notice naming any problem with the file.
fn load_config(volume: Result<&mut FileSystem, Status>) -> (Config, Option<String>) {
    let unusable = |problem: String| {
        let config = Config {
            default: None,
            timeout: None,
            entries: Vec::new(),
            detect: false,
        };
        (
            config,
            Some(format!("Couldn't use {ENTRIES_PATH}: {problem}")),
        )
    };
    let volume = match volume {
        Ok(volume) => volume,
        Err(status) => return unusable(format!("{status:?}")),
    };
    match volume.read(cstr16!("\\EFI\\Mikros\\entries.yaml")) {
        Ok(bytes) => match Config::parse(&bytes) {
            Ok(config) => (config, None),
            Err(err) => unusable(format!("{err}")),
        },
        Err(uefi::fs::Error::Io(err)) if err.uefi_error.status() == Status::NOT_FOUND => {
            (Config::for_missing_file(), None)
        }
        Err(err) => unusable(format!("{err}")),
    }
}

/// Whether the firmware can be asked to show its setup screen.
fn firmware_ui_supported() -> bool {
    let mut buf = [0u8; 8];
    let name = cstr16!("OsIndicationsSupported");
    match runtime::get_variable(name, &VariableVendor::GLOBAL_VARIABLE, &mut buf) {
        Ok((value, _)) if value.len() == 8 => u64::from_le_bytes(buf) & BOOT_TO_FW_UI != 0,
        _ => false,
    }
}

/// Reads the BMP image at `path` on the boot volume and scales it to fit
/// the tile.
fn load_icon(volume: Option<&mut FileSystem>, path: &str) -> Result<bmp::Image, String> {
    let volume = volume.ok_or_else(|| String::from("the boot volume was not found"))?;
    let name = CString16::try_from(path).map_err(|_| String::from("the path is not valid"))?;
    let bytes = volume.read(name.as_ref()).map_err(|err| match err {
        uefi::fs::Error::Io(err) if err.uefi_error.status() == Status::NOT_FOUND => {
            String::from("the file was not found")
        }
        uefi::fs::Error::Io(err) => format!("{:?}", err.uefi_error.status()),
        err => format!("{err}"),
    })?;
    let image = bmp::decode(&bytes).map_err(|err| match err {
        bmp::Error::NotBmp => String::from("not a BMP image"),
        bmp::Error::Truncated => String::from("the image is truncated"),
        bmp::Error::Unsupported => String::from("unsupported BMP format"),
    })?;
    Ok(image.fit_within(ICON_SIZE))
}

/// Builds the menu for `config`. Notices about the entries file and about
/// icons that could not be loaded are joined into the menu's notice.
fn build_menu(
    config: &Config,
    mut volume: Option<&mut FileSystem>,
    config_notice: Option<String>,
) -> Menu {
    let mut notices: Vec<String> = config_notice.into_iter().collect();
    let cards = config
        .entries
        .iter()
        .map(|entry| {
            let icon = entry.icon.as_deref().and_then(|path| {
                match load_icon(volume.as_deref_mut(), path) {
                    Ok(image) => Some(image),
                    Err(reason) => {
                        notices.push(format!(
                            "Couldn't load the icon {path} of {}: {reason}",
                            entry.name
                        ));
                        None
                    }
                }
            });
            Card {
                name: entry.name.clone(),
                detail: entry.detail.clone(),
                color: Color::rgb(entry.color.r, entry.color.g, entry.color.b),
                icon,
            }
        })
        .collect::<Vec<_>>();
    let notice = (!notices.is_empty()).then(|| notices.join("; "));
    let mut buttons = vec![Power::Restart, Power::ShutDown];
    if firmware_ui_supported() {
        buttons.push(Power::FirmwareSettings);
    }
    let focus =
        (!cards.is_empty()).then(|| (Item::Card(config.default_index()), FocusSource::Keyboard));
    Menu {
        cards,
        buttons,
        notice,
        focus,
        countdown: None,
    }
}

/// The bootloader, started when there is no entry to start instead.
const MIKROSBOOT_PATH: &str = "\\EFI\\Mikros\\mikrosboot.efi";

/// Translates a key from the console into a menu event.
fn translate(key: Key) -> Event {
    match key {
        Key::Special(ScanCode::RIGHT | ScanCode::DOWN) => Event::Next,
        Key::Special(ScanCode::LEFT | ScanCode::UP) => Event::Previous,
        Key::Printable(c) => match char::from(c) {
            '\t' => Event::Next,
            '\r' | '\n' | ' ' => Event::Activate,
            _ => Event::OtherKey,
        },
        Key::Special(_) => Event::OtherKey,
    }
}

/// Whether a key is held while the boot manager starts. Reads, and so
/// discards, every pending key stroke, including Shift on its own where the
/// console reports it.
fn key_held() -> bool {
    let Some(mut input) = pointers::open_on_console::<InputEx>() else {
        return false;
    };
    let _ = input.set_state(KeyToggleState::VALID | KeyToggleState::EXPOSED);
    let mut held = false;
    while let Ok(Some(_)) = input.read_key() {
        held = true;
    }
    held
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
    let _ = boot::set_watchdog_timer(0, 0x10000, None);
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

/// Starts `path` on the boot volume with `args`.
fn start(volume: Option<&DevicePath>, path: &str, args: Option<&str>) -> Result<(), String> {
    let volume = volume.ok_or_else(|| String::from("the boot volume was not found"))?;
    launch::start(volume, path, args)
}

pub fn run() -> Status {
    let mut file_system = boot::get_image_file_system(boot::image_handle())
        .map(FileSystem::new)
        .map_err(|err| err.status());
    let (config, config_notice) = load_config(file_system.as_mut().map_err(|s| *s));
    let mut menu = build_menu(&config, file_system.as_mut().ok(), config_notice);
    drop(file_system);
    let startup_notice = menu.notice.clone();
    let volume = launch::boot_volume();
    let default = config.default_index();
    let Some(mut text) = Text::new() else {
        return Status::LOAD_ERROR;
    };

    // Decide what happens at start-up: start the default entry at once, show
    // the menu with a countdown, or show it without one.
    let mut start_now = None;
    if let Some(entry) = config.entries.get(default)
        && !key_held()
    {
        match config.timeout {
            Some(0) => start_now = Some(Item::Card(default)),
            Some(seconds) => menu.countdown = Some((entry.name.clone(), seconds)),
            None => {}
        }
    }

    loop {
        let item = match start_now.take() {
            Some(item) => item,
            None => {
                let Some(screen) = Screen::open() else {
                    let (path, args) = match config.entries.get(default) {
                        Some(entry) => (entry.path.as_str(), entry.args.as_deref()),
                        None => (MIKROSBOOT_PATH, None),
                    };
                    return match start(volume.as_deref(), path, args) {
                        Ok(()) => Status::SUCCESS,
                        Err(_) => Status::LOAD_ERROR,
                    };
                };
                show(&mut menu, screen, &mut text, default)
            }
        };
        match item {
            Item::Button(power) => power::act(power),
            Item::Card(index) => {
                let entry = &config.entries[index];
                let result = start(volume.as_deref(), &entry.path, entry.args.as_deref());
                menu.notice = match result {
                    Ok(()) => startup_notice.clone(),
                    Err(reason) => Some(format!("Couldn't start {}: {reason}", entry.name)),
                };
                menu.focus = Some((Item::Card(index), FocusSource::Keyboard));
                menu.countdown = None;
            }
        }
    }
}
