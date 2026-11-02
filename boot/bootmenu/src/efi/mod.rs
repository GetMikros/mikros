//! The boot manager's use of UEFI.

mod launch;
mod power;
mod screen;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bootmenu::canvas::{Canvas, Color};
use bootmenu::entries::Config;
use bootmenu::input::{Action, Event, Input};
use bootmenu::menu::{Card, FocusSource, Item, Menu, Power};
use bootmenu::text::Text;
use core::time::Duration;
use screen::Screen;
use uefi::boot::{EventType, OpenProtocolAttributes, OpenProtocolParams, TimerTrigger, Tpl};
use uefi::fs::FileSystem;
use uefi::proto::console::text::{InputEx, Key, ScanCode};
use uefi::proto::device_path::DevicePath;
use uefi::runtime::{self, VariableVendor};
use uefi::{Handle, Status, boot, cstr16, system, table};
use uefi_raw::protocol::console::KeyToggleState;

const ENTRIES_PATH: &str = "\\EFI\\Mikros\\entries.yaml";

/// Bit of `OsIndicationsSupported` and `OsIndications` that asks the
/// firmware to show its setup screen on the next boot.
const BOOT_TO_FW_UI: u64 = 1;

/// Reads the entries file from the volume the boot manager was loaded from.
/// Returns the configuration and a notice naming any problem with the file.
fn load_config() -> (Config, Option<String>) {
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
    let volume = match boot::get_image_file_system(boot::image_handle()) {
        Ok(volume) => volume,
        Err(err) => return unusable(format!("{:?}", err.status())),
    };
    match FileSystem::new(volume).read(cstr16!("\\EFI\\Mikros\\entries.yaml")) {
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

fn build_menu(config: &Config, notice: Option<String>) -> Menu {
    let cards = config
        .entries
        .iter()
        .map(|entry| Card {
            name: entry.name.clone(),
            detail: entry.detail.clone(),
            color: Color::rgb(entry.color.r, entry.color.g, entry.color.b),
        })
        .collect::<Vec<_>>();
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
    let console = table::system_table_raw().and_then(|st| {
        // SAFETY: the system table pointer is valid while boot services run,
        // and its console input handle is either null or a valid handle.
        unsafe { Handle::from_ptr(st.as_ref().stdin_handle) }
    });
    let Some(handle) = console else {
        return false;
    };
    let params = OpenProtocolParams {
        handle,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: GetProtocol does not take ownership of the protocol, so the
    // console keeps using it; it is closed again when this function returns.
    let opened =
        unsafe { boot::open_protocol::<InputEx>(params, OpenProtocolAttributes::GetProtocol) };
    let Ok(mut input) = opened else {
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

/// Waits for the next key press or tick of `timer`, and translates it.
fn next_event(timer: Option<&uefi::Event>) -> Event {
    system::with_stdin(|input| {
        loop {
            if let Ok(key) = input.wait_for_key_event() {
                // SAFETY: the clone is only used for this wait, while the
                // timer event is still open.
                let events: Vec<uefi::Event> = core::iter::once(key)
                    .chain(timer.map(|t| unsafe { t.unsafe_clone() }))
                    .collect();
                if let Ok(1) = boot::wait_for_event(&events) {
                    return Event::Second;
                }
            }
            match input.read_key() {
                Ok(Some(key)) => return translate(key),
                Ok(None) => continue,
                Err(_) => return Event::OtherKey,
            }
        }
    })
}

/// Shows `menu` on `screen` until an item is activated, and returns it. The
/// screen is released when this returns.
fn show(menu: &mut Menu, mut screen: Screen, text: &mut Text, default: usize) -> Item {
    let _ = boot::set_watchdog_timer(0, 0x10000, None);
    let (width, height) = screen.size();
    let mut canvas = Canvas::new(width, height);
    let mut input = Input::new(width as i32, height as i32, default);
    let mut timer = menu.countdown.as_ref().and_then(|_| second_timer());
    menu.draw(text, &mut canvas);
    screen.present(&canvas);
    let item = loop {
        let action = input.handle(menu, next_event(timer.as_ref()));
        if menu.countdown.is_none()
            && let Some(event) = timer.take()
        {
            let _ = boot::close_event(event);
        }
        match action {
            Action::None => {}
            Action::Redraw => {
                menu.draw(text, &mut canvas);
                screen.present(&canvas);
            }
            Action::Activate(item) => break item,
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
    let (config, config_notice) = load_config();
    let mut menu = build_menu(&config, config_notice.clone());
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
                    Ok(()) => config_notice.clone(),
                    Err(reason) => Some(format!("Couldn't start {}: {reason}", entry.name)),
                };
                menu.focus = Some((Item::Card(index), FocusSource::Keyboard));
                menu.countdown = None;
            }
        }
    }
}
