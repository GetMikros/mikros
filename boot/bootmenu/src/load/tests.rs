use super::*;
use crate::entries::Entry;
use alloc::string::ToString;

fn entry(id: &str, icon: Option<&str>) -> Entry {
    Entry {
        id: id.to_string(),
        name: alloc::format!("Entry {id}"),
        detail: None,
        color: entries::Color { r: 1, g: 2, b: 3 },
        icon: icon.map(String::from),
        path: alloc::format!("\\EFI\\{id}.efi"),
        args: None,
    }
}

fn config(entries: Vec<Entry>) -> Config {
    Config {
        default: None,
        timeout: None,
        entries,
        detect: true,
    }
}

fn detected(name: &str, other_disk: bool) -> Detected<()> {
    Detected {
        id: String::from("detected-1"),
        name: name.to_string(),
        path: String::from("\\EFI\\x.efi"),
        other_disk,
        device_path: (),
    }
}

/// A 1×1 24-bit BMP file.
fn one_pixel_bmp() -> Vec<u8> {
    let mut out = b"BM".to_vec();
    for v in [0u32, 0, 54, 40, 1, 1] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(&[0x30, 0x20, 0x10, 0]);
    out
}

fn no_icons(path: &str) -> Result<bmp::Image, String> {
    panic!("icon {path} loaded")
}

#[test]
fn uses_the_default_configuration_for_a_missing_file() {
    let loaded = load_config(Err(ReadError::NotFound));
    assert_eq!(loaded.config, Config::for_missing_file());
    assert_eq!(loaded.notice, None);
    assert!(loaded.missing);
}

#[test]
fn counts_down_5_seconds_when_systems_are_added_to_a_missing_file() {
    let mut loaded = load_config(Err(ReadError::NotFound));
    apply_detection(&mut loaded, 0);
    assert_eq!(loaded.config.timeout, Some(0), "nothing detected");
    apply_detection(&mut loaded, 2);
    assert_eq!(loaded.config.timeout, Some(5));

    let mut present = load_config(Ok(b"timeout: 0\n".to_vec()));
    apply_detection(&mut present, 2);
    assert_eq!(present.config.timeout, Some(0), "file present");
}

#[test]
fn reports_files_that_cannot_be_used() {
    let cases = [
        (
            Err(ReadError::Other(String::from("DEVICE_ERROR"))),
            "DEVICE_ERROR",
        ),
        (
            Ok(b"timeout: 5\n\xff\n".to_vec()),
            "the file is not valid UTF-8",
        ),
        (
            Ok(b"timeout: 5\ndetect: maybe\n".to_vec()),
            "line 2: `detect` must be `true` or `false`",
        ),
    ];
    for (file, problem) in cases {
        let loaded = load_config(file);
        assert_eq!(
            loaded.notice,
            Some(alloc::format!("Couldn't use {ENTRIES_PATH}: {problem}"))
        );
        assert!(loaded.config.entries.is_empty(), "{problem}");
        assert_eq!(loaded.config.timeout, None, "{problem}");
        assert!(!loaded.config.detect, "{problem}");
        assert!(!loaded.missing, "{problem}");

        let menu = build_menu::<()>(&loaded.config, &[], no_icons, loaded.notice, false);
        assert!(menu.cards.is_empty());
        assert_eq!(menu.focus, None);
        assert!(menu.notice.unwrap().ends_with(problem));
    }
}

#[test]
fn describes_icons_that_cannot_be_loaded() {
    assert_eq!(
        icon(Err(ReadError::NotFound)),
        Err(String::from("the file was not found"))
    );
    assert_eq!(
        icon(Err(ReadError::Other(String::from("DEVICE_ERROR")))),
        Err(String::from("DEVICE_ERROR"))
    );
    assert_eq!(
        icon(Ok(b"GIF89a".to_vec())),
        Err(String::from("not a BMP image"))
    );
    let image = icon(Ok(one_pixel_bmp())).unwrap();
    assert_eq!((image.width, image.height), (1, 1));
}

#[test]
fn shows_the_tile_and_a_notice_when_an_icon_fails() {
    let config = config(vec![
        entry("a", Some("\\a.bmp")),
        entry("b", Some("\\b.bmp")),
        entry("c", None),
    ]);
    let mut loaded = Vec::new();
    let menu = build_menu::<()>(
        &config,
        &[],
        |path| {
            loaded.push(path.to_string());
            match path {
                "\\a.bmp" => icon(Err(ReadError::NotFound)),
                _ => icon(Ok(one_pixel_bmp())),
            }
        },
        Some(String::from("first")),
        false,
    );
    assert_eq!(loaded, ["\\a.bmp", "\\b.bmp"]);
    assert!(menu.cards[0].icon.is_none());
    assert_eq!(menu.cards[0].color, Color::rgb(1, 2, 3));
    assert!(menu.cards[1].icon.is_some());
    assert_eq!(
        menu.notice.as_deref(),
        Some("first; Couldn't load the icon \\a.bmp of Entry a: the file was not found")
    );
}

#[test]
fn adds_detected_systems_after_the_entries() {
    let mut config = config(vec![entry("a", None), entry("b", None)]);
    config.default = Some(String::from("b"));
    let detected = [detected("Windows", false), detected("Fedora", true)];
    let menu = build_menu(&config, &detected, no_icons, None, false);
    let names: Vec<&str> = menu.cards.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Entry a", "Entry b", "Windows", "Fedora"]);
    assert_eq!(menu.cards[2].detail, None);
    assert_eq!(menu.cards[3].detail.as_deref(), Some("Other disk"));
    let tile = entries::Color::DEFAULT_TILE;
    assert_eq!(menu.cards[3].color, Color::rgb(tile.r, tile.g, tile.b));
    assert_eq!(menu.focus, Some((Item::Card(1), FocusSource::Keyboard)));
    assert_eq!(menu.notice, None);
    assert_eq!(menu.countdown, None);
}

#[test]
fn offers_firmware_settings_only_when_supported() {
    let config = config(vec![entry("a", None)]);
    let without = build_menu::<()>(&config, &[], no_icons, None, false);
    assert_eq!(without.buttons, [Power::Restart, Power::ShutDown]);
    let with = build_menu::<()>(&config, &[], no_icons, None, true);
    assert_eq!(
        with.buttons,
        [Power::Restart, Power::ShutDown, Power::FirmwareSettings]
    );
}
