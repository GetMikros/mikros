//! The boot manager's use of UEFI.

mod screen;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bootmenu::canvas::{Canvas, Color};
use bootmenu::entries::Config;
use bootmenu::menu::{Card, FocusSource, Item, Menu, Power};
use bootmenu::text::Text;
use screen::Screen;
use uefi::fs::FileSystem;
use uefi::runtime::{self, VariableVendor};
use uefi::{Status, boot, cstr16, system};

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

pub fn run() -> Status {
    let (config, notice) = load_config();
    let menu = build_menu(&config, notice);

    let Some(mut screen) = Screen::open() else {
        return Status::UNSUPPORTED;
    };
    let Some(mut text) = Text::new() else {
        return Status::LOAD_ERROR;
    };
    let _ = boot::set_watchdog_timer(0, 0x10000, None);

    let (width, height) = screen.size();
    let mut canvas = Canvas::new(width, height);
    menu.draw(&mut text, &mut canvas);
    screen.present(&canvas);

    system::with_stdin(|input| {
        if let Ok(event) = input.wait_for_key_event() {
            let _ = boot::wait_for_event(&[event]);
        }
    });
    Status::SUCCESS
}
