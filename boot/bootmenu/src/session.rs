//! What the boot manager does once the menu is built: starting the default
//! entry at once or showing the menu, starting the chosen entry, and
//! showing the menu again when it returns.

use crate::entries::Config;
use crate::menu::{FocusSource, Item, Menu, Power};
use alloc::format;
use alloc::string::String;

/// The bootloader, started when there is no entry to start instead.
pub const MIKROSBOOT_PATH: &str = "\\EFI\\Mikros\\mikrosboot.efi";

/// The firmware services the boot manager uses after building the menu.
pub trait Firmware {
    /// A screen in a mode the menu can be drawn in.
    type Screen;

    /// Whether a key is held while the boot manager starts, as `key_held`
    /// tells.
    fn key_held(&mut self) -> bool;
    /// Opens the screen, or returns `None` when no display mode is usable.
    fn open_screen(&mut self) -> Option<Self::Screen>;
    /// Turns the watchdog timer off.
    fn disable_watchdog(&mut self);
    /// Shows `menu` on `screen` until an item is activated, and returns it.
    /// The screen and the input devices are released when this returns.
    /// `default` is the entry the countdown starts.
    fn show(&mut self, menu: &mut Menu, screen: Self::Screen, default: usize) -> Item;
    /// Starts the image at `path` on the boot volume with `args`. Returns
    /// when the image exits, or with the reason it could not be started.
    fn start_file(&mut self, path: &str, args: Option<&str>) -> Result<(), String>;
    /// Starts the detected system with the given index, as `start_file`.
    fn start_detected(&mut self, index: usize) -> Result<(), String>;
}

/// Why `run` returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// A power button was activated.
    Power(Power),
    /// No display mode was usable, so an image was started without showing
    /// the menu, and it returned with this result.
    NoScreen(Result<(), String>),
}

/// Runs the boot manager with `menu`, built for `config` followed by the
/// detected systems.
pub fn run<F: Firmware>(firmware: &mut F, config: &Config, mut menu: Menu) -> Exit {
    let startup_notice = menu.notice.clone();
    let default = config.default_index();

    // Decide what happens at start-up: start the default entry at once, show
    // the menu with a countdown, or show it without one.
    let mut start_now = None;
    if let Some(entry) = config.entries.get(default)
        && !firmware.key_held()
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
                let Some(screen) = firmware.open_screen() else {
                    let (path, args) = match config.entries.get(default) {
                        Some(entry) => (entry.path.as_str(), entry.args.as_deref()),
                        None => (MIKROSBOOT_PATH, None),
                    };
                    return Exit::NoScreen(firmware.start_file(path, args));
                };
                firmware.disable_watchdog();
                firmware.show(&mut menu, screen, default)
            }
        };
        match item {
            Item::Button(power) => return Exit::Power(power),
            Item::Card(index) => {
                let result = match config.entries.get(index) {
                    Some(entry) => firmware.start_file(&entry.path, entry.args.as_deref()),
                    None => firmware.start_detected(index - config.entries.len()),
                };
                menu.notice = match result {
                    Ok(()) => startup_notice.clone(),
                    Err(reason) => Some(format!(
                        "Couldn't start {}: {reason}",
                        menu.cards[index].name
                    )),
                };
                menu.focus = Some((Item::Card(index), FocusSource::Keyboard));
                menu.countdown = None;
            }
        }
    }
}

/// A key stroke read from the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub scan_code: u16,
    pub unicode_char: u16,
    /// The shift state bits, or 0 when the console reports none.
    pub shift_state: u32,
}

/// The console input as `key_held` uses it.
pub trait Keyboard {
    /// Asks the console to also report keys such as Shift that are pressed
    /// on their own. Consoles that cannot do this are left as they are.
    fn expose_key_state(&mut self);
    /// Reads the next pending key stroke. Returns `None` when there is none
    /// or it cannot be read.
    fn read_key_stroke(&mut self) -> Option<KeyStroke>;
}

/// Whether a key is held while the boot manager starts, which is the case
/// when `keyboard` has any key stroke pending. Reads, and so discards, every
/// pending key stroke.
pub fn key_held(keyboard: Option<&mut impl Keyboard>) -> bool {
    let Some(keyboard) = keyboard else {
        return false;
    };
    keyboard.expose_key_state();
    let mut held = false;
    while keyboard.read_key_stroke().is_some() {
        held = true;
    }
    held
}

#[cfg(test)]
mod tests;
