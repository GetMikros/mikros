//! Reading the entries file and the icons, and building the menu from them.
//! The UEFI code reads the files and passes in their contents.

use crate::bmp;
use crate::canvas::Color;
use crate::detect::Detected;
use crate::entries::{self, Config};
use crate::menu::{Card, FocusSource, ICON_SIZE, Item, Menu, Power};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const ENTRIES_PATH: &str = "\\EFI\\Mikros\\entries.yaml";

/// Why a file could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    NotFound,
    /// Any other problem, described for a notice.
    Other(String),
}

/// The entries file as loaded.
#[derive(Debug, PartialEq, Eq)]
pub struct Loaded {
    pub config: Config,
    /// A notice naming any problem with the file.
    pub notice: Option<String>,
    /// Whether the file is missing.
    pub missing: bool,
}

/// Turns the result of reading the entries file into the configuration.
pub fn load_config(file: Result<Vec<u8>, ReadError>) -> Loaded {
    let unusable = |problem: String| Loaded {
        config: Config {
            default: None,
            timeout: None,
            entries: Vec::new(),
            detect: false,
        },
        notice: Some(format!("Couldn't use {ENTRIES_PATH}: {problem}")),
        missing: false,
    };
    match file {
        Ok(bytes) => match Config::parse(&bytes) {
            Ok(config) => Loaded {
                config,
                notice: None,
                missing: false,
            },
            Err(err) => unusable(format!("{err}")),
        },
        Err(ReadError::NotFound) => Loaded {
            config: Config::for_missing_file(),
            notice: None,
            missing: true,
        },
        Err(ReadError::Other(problem)) => unusable(problem),
    }
}

/// Sets the countdown of a missing entries file to 5 seconds when
/// `detected` systems were added to it.
pub fn apply_detection(loaded: &mut Loaded, detected: usize) {
    if loaded.missing && detected > 0 {
        loaded.config.timeout = Some(5);
    }
}

/// Turns the result of reading an icon into the image to show, scaled to
/// fit the tile, or the reason it cannot be shown.
pub fn icon(file: Result<Vec<u8>, ReadError>) -> Result<bmp::Image, String> {
    let bytes = file.map_err(|err| match err {
        ReadError::NotFound => String::from("the file was not found"),
        ReadError::Other(problem) => problem,
    })?;
    let image = bmp::decode(&bytes).map_err(|err| match err {
        bmp::Error::NotBmp => String::from("not a BMP image"),
        bmp::Error::Truncated => String::from("the image is truncated"),
        bmp::Error::Unsupported => String::from("unsupported BMP format"),
    })?;
    Ok(image.fit_within(ICON_SIZE))
}

/// Builds the menu for `config`, followed by the `detected` systems.
/// `load_icon` loads the icon at a path, as `icon` does. The notice about
/// the entries file and those about icons that could not be loaded are
/// joined into the menu's notice. `firmware_ui` adds the firmware settings
/// button.
pub fn build_menu<P>(
    config: &Config,
    detected: &[Detected<P>],
    mut load_icon: impl FnMut(&str) -> Result<bmp::Image, String>,
    config_notice: Option<String>,
    firmware_ui: bool,
) -> Menu {
    let mut notices: Vec<String> = config_notice.into_iter().collect();
    let cards = config
        .entries
        .iter()
        .map(|entry| {
            let icon = entry
                .icon
                .as_deref()
                .and_then(|path| match load_icon(path) {
                    Ok(image) => Some(image),
                    Err(reason) => {
                        notices.push(format!(
                            "Couldn't load the icon {path} of {}: {reason}",
                            entry.name
                        ));
                        None
                    }
                });
            Card {
                name: entry.name.clone(),
                detail: entry.detail.clone(),
                color: Color::rgb(entry.color.r, entry.color.g, entry.color.b),
                icon,
            }
        })
        .chain(detected.iter().map(|system| Card {
            name: system.name.clone(),
            detail: system.other_disk.then(|| String::from("Other disk")),
            color: Color::rgb(
                entries::Color::DEFAULT_TILE.r,
                entries::Color::DEFAULT_TILE.g,
                entries::Color::DEFAULT_TILE.b,
            ),
            icon: None,
        }))
        .collect::<Vec<_>>();
    let notice = (!notices.is_empty()).then(|| notices.join("; "));
    let mut buttons = vec![Power::Restart, Power::ShutDown];
    if firmware_ui {
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

#[cfg(test)]
mod tests;
