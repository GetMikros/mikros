use super::*;
use crate::canvas::Color;
use crate::menu::Card;
use alloc::collections::VecDeque;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

/// A call to the fake firmware.
#[derive(Debug, PartialEq, Eq)]
enum Call {
    KeyHeld,
    OpenScreen,
    DisableWatchdog,
    /// The countdown, notice and focus of the menu shown.
    Show {
        countdown: Option<(String, u32)>,
        notice: Option<String>,
        focus: Option<(Item, FocusSource)>,
    },
    StartFile(String, Option<String>),
    StartDetected(usize),
}

/// A firmware that records the calls made to it and answers them from
/// scripted results.
struct FakeFirmware {
    calls: Vec<Call>,
    key_held: bool,
    /// Whether each screen opened is usable; usable once these run out.
    screens: VecDeque<bool>,
    /// The items activated in each menu shown.
    items: VecDeque<Item>,
    /// The results of each image started; success once these run out.
    results: VecDeque<Result<(), String>>,
}

impl FakeFirmware {
    fn new(items: &[Item]) -> FakeFirmware {
        FakeFirmware {
            calls: Vec::new(),
            key_held: false,
            screens: VecDeque::new(),
            items: items.iter().copied().collect(),
            results: VecDeque::new(),
        }
    }
}

impl Firmware for FakeFirmware {
    type Screen = ();

    fn key_held(&mut self) -> bool {
        self.calls.push(Call::KeyHeld);
        self.key_held
    }

    fn open_screen(&mut self) -> Option<()> {
        self.calls.push(Call::OpenScreen);
        self.screens.pop_front().unwrap_or(true).then_some(())
    }

    fn disable_watchdog(&mut self) {
        self.calls.push(Call::DisableWatchdog);
    }

    fn show(&mut self, menu: &mut Menu, _screen: (), _default: usize) -> Item {
        self.calls.push(Call::Show {
            countdown: menu.countdown.clone(),
            notice: menu.notice.clone(),
            focus: menu.focus,
        });
        self.items.pop_front().expect("no item left to activate")
    }

    fn start_file(&mut self, path: &str, args: Option<&str>) -> Result<(), String> {
        self.calls
            .push(Call::StartFile(path.to_string(), args.map(String::from)));
        self.results.pop_front().unwrap_or(Ok(()))
    }

    fn start_detected(&mut self, index: usize) -> Result<(), String> {
        self.calls.push(Call::StartDetected(index));
        self.results.pop_front().unwrap_or(Ok(()))
    }
}

/// The configuration of `text`, and a menu with its entries and `detected`
/// more cards, focused as the menu is built.
fn setup(text: &str, detected: usize) -> (Config, Menu) {
    let config = Config::parse(text.as_bytes()).unwrap();
    let names = config
        .entries
        .iter()
        .map(|e| e.name.clone())
        .chain((0..detected).map(|i| alloc::format!("Detected {i}")));
    let cards: Vec<Card> = names
        .map(|name| Card {
            name,
            detail: None,
            color: Color(0),
            icon: None,
        })
        .collect();
    let focus =
        (!cards.is_empty()).then(|| (Item::Card(config.default_index()), FocusSource::Keyboard));
    let menu = Menu {
        cards,
        buttons: vec![Power::Restart, Power::ShutDown],
        notice: None,
        focus,
        countdown: None,
    };
    (config, menu)
}

const TWO_ENTRIES: &str = "default: b
timeout: 3
entries:
  - id: a
    name: A
    path: /EFI/a.efi
  - id: b
    name: B
    path: /EFI/b.efi
    args: \"fast quiet\"
";

fn start(path: &str, args: Option<&str>) -> Call {
    Call::StartFile(path.to_string(), args.map(String::from))
}

fn show(countdown: Option<(&str, u32)>, notice: Option<&str>, card: usize) -> Call {
    Call::Show {
        countdown: countdown.map(|(name, n)| (name.to_string(), n)),
        notice: notice.map(String::from),
        focus: Some((Item::Card(card), FocusSource::Keyboard)),
    }
}

#[test]
fn starts_the_default_entry_without_a_usable_screen() {
    let (config, menu) = setup(TWO_ENTRIES, 0);
    let mut firmware = FakeFirmware::new(&[]);
    firmware.screens.push_back(false);
    firmware.results.push_back(Err(String::from("x")));
    let exit = run(&mut firmware, &config, menu);
    assert_eq!(exit, Exit::NoScreen(Err(String::from("x"))));
    assert_eq!(
        firmware.calls,
        [
            Call::KeyHeld,
            Call::OpenScreen,
            start("\\EFI\\b.efi", Some("fast quiet"))
        ]
    );
}

#[test]
fn starts_mikrosboot_without_entries_or_a_usable_screen() {
    let (config, menu) = setup("timeout: 3\n", 0);
    let mut firmware = FakeFirmware::new(&[]);
    firmware.screens.push_back(false);
    assert_eq!(run(&mut firmware, &config, menu), Exit::NoScreen(Ok(())));
    assert_eq!(
        firmware.calls,
        [Call::OpenScreen, start(MIKROSBOOT_PATH, None)]
    );
}

#[test]
fn disables_the_watchdog_before_showing_the_menu() {
    let (config, menu) = setup(TWO_ENTRIES, 0);
    let mut firmware = FakeFirmware::new(&[Item::Card(0), Item::Button(Power::Restart)]);
    run(&mut firmware, &config, menu);
    let shows: Vec<usize> = (0..firmware.calls.len())
        .filter(|&i| matches!(firmware.calls[i], Call::Show { .. }))
        .collect();
    assert_eq!(shows.len(), 2);
    for i in shows {
        assert_eq!(firmware.calls[i - 1], Call::DisableWatchdog);
        assert_eq!(firmware.calls[i - 2], Call::OpenScreen);
    }
}

#[test]
fn counts_down_unless_a_key_is_held() {
    let (config, menu) = setup(TWO_ENTRIES, 0);
    let mut firmware = FakeFirmware::new(&[Item::Button(Power::ShutDown)]);
    let exit = run(&mut firmware, &config, menu);
    assert_eq!(exit, Exit::Power(Power::ShutDown));
    assert_eq!(firmware.calls[3], show(Some(("B", 3)), None, 1));

    let (config, menu) = setup(TWO_ENTRIES, 0);
    let mut firmware = FakeFirmware::new(&[Item::Button(Power::FirmwareSettings)]);
    firmware.key_held = true;
    let exit = run(&mut firmware, &config, menu);
    assert_eq!(exit, Exit::Power(Power::FirmwareSettings));
    assert_eq!(firmware.calls[3], show(None, None, 1));
}

#[test]
fn starts_at_once_with_timeout_0_and_shows_the_menu_when_the_entry_returns() {
    let text = TWO_ENTRIES.replace("timeout: 3", "timeout: 0");
    let (config, menu) = setup(&text, 0);
    let mut firmware = FakeFirmware::new(&[Item::Button(Power::Restart)]);
    run(&mut firmware, &config, menu);
    assert_eq!(
        firmware.calls,
        [
            Call::KeyHeld,
            start("\\EFI\\b.efi", Some("fast quiet")),
            Call::OpenScreen,
            Call::DisableWatchdog,
            show(None, None, 1),
        ]
    );

    let (config, menu) = setup(&text, 0);
    let mut firmware = FakeFirmware::new(&[Item::Button(Power::Restart)]);
    firmware.key_held = true;
    run(&mut firmware, &config, menu);
    assert_eq!(firmware.calls[1], Call::OpenScreen, "key held");
}

#[test]
fn returns_to_the_menu_with_the_reason_an_entry_failed() {
    let (config, mut menu) = setup(TWO_ENTRIES, 2);
    menu.notice = Some(String::from("startup"));
    let mut firmware = FakeFirmware::new(&[
        Item::Card(0),
        Item::Card(3),
        Item::Card(2),
        Item::Button(Power::Restart),
    ]);
    firmware.results = [
        Err(String::from("\\EFI\\a.efi not found")),
        Err(String::from("LOAD_ERROR")),
        Ok(()),
    ]
    .into_iter()
    .collect();
    run(&mut firmware, &config, menu);
    let calls: Vec<&Call> = firmware
        .calls
        .iter()
        .filter(|c| !matches!(c, Call::OpenScreen | Call::DisableWatchdog))
        .collect();
    assert_eq!(
        calls,
        [
            &Call::KeyHeld,
            &show(Some(("B", 3)), Some("startup"), 1),
            &start("\\EFI\\a.efi", None),
            &show(None, Some("Couldn't start A: \\EFI\\a.efi not found"), 0),
            &Call::StartDetected(1),
            &show(None, Some("Couldn't start Detected 1: LOAD_ERROR"), 3),
            &Call::StartDetected(0),
            &show(None, Some("startup"), 2),
        ]
    );
}

#[test]
fn shows_no_countdown_without_entries() {
    let (config, menu) = setup("timeout: 3\n", 0);
    let mut firmware = FakeFirmware::new(&[Item::Button(Power::Restart)]);
    run(&mut firmware, &config, menu);
    assert_eq!(
        firmware.calls[2],
        Call::Show {
            countdown: None,
            notice: None,
            focus: None
        }
    );
}

/// A console with pending key strokes.
struct FakeKeyboard {
    strokes: VecDeque<KeyStroke>,
    exposed: bool,
    /// Whether a stroke was read before the key state was exposed.
    read_unexposed: bool,
}

impl Keyboard for FakeKeyboard {
    fn expose_key_state(&mut self) {
        self.exposed = true;
    }

    fn read_key_stroke(&mut self) -> Option<KeyStroke> {
        self.read_unexposed |= !self.exposed;
        self.strokes.pop_front()
    }
}

fn keyboard(strokes: &[KeyStroke]) -> FakeKeyboard {
    FakeKeyboard {
        strokes: strokes.iter().copied().collect(),
        exposed: false,
        read_unexposed: false,
    }
}

#[test]
fn treats_any_pending_key_stroke_as_held_and_discards_them() {
    let enter = KeyStroke {
        scan_code: 0,
        unicode_char: u16::from(b'\r'),
        shift_state: 0,
    };
    // Left Shift on its own, with the shift state valid.
    let shift = KeyStroke {
        scan_code: 0,
        unicode_char: 0,
        shift_state: 0x8000_0000 | 0x2,
    };
    for strokes in [&[shift][..], &[enter, shift, enter]] {
        let mut keyboard = keyboard(strokes);
        assert!(key_held(Some(&mut keyboard)));
        assert!(keyboard.strokes.is_empty(), "all strokes read");
        assert!(keyboard.exposed);
        assert!(!keyboard.read_unexposed);
    }

    let mut keyboard = keyboard(&[]);
    assert!(!key_held(Some(&mut keyboard)));
    assert!(keyboard.exposed);
    assert!(!key_held(None::<&mut FakeKeyboard>), "no console input");
}
