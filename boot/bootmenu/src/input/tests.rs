use super::*;
use crate::canvas::Color;
use crate::menu::{Card, Power};
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

const W: i32 = 1920;
const H: i32 = 1080;

fn menu(cards: usize) -> Menu {
    Menu {
        cards: (0..cards)
            .map(|i| Card {
                name: alloc::format!("Entry {i}"),
                detail: None,
                color: Color(0),
                icon: None,
            })
            .collect(),
        buttons: vec![Power::Restart, Power::ShutDown],
        notice: None,
        focus: (cards > 0).then_some((Item::Card(0), FocusSource::Keyboard)),
        countdown: None,
    }
}

fn focus(menu: &Menu) -> Option<Item> {
    menu.focus.map(|(item, _)| item)
}

#[test]
fn translates_the_keys_of_the_focus_order() {
    for key in [Key::Right, Key::Char('\t')] {
        assert_eq!(translate(key), Event::Next, "{key:?}");
    }
    assert_eq!(translate(Key::Left), Event::Previous);
    assert_eq!(translate(Key::Down), Event::Down);
    assert_eq!(translate(Key::Up), Event::Up);
    for key in [Key::Char('\r'), Key::Char('\n'), Key::Char(' ')] {
        assert_eq!(translate(key), Event::Activate, "{key:?}");
    }
    for key in [Key::Char('a'), Key::Char('\u{1b}'), Key::OtherSpecial] {
        assert_eq!(translate(key), Event::OtherKey, "{key:?}");
    }
}

#[test]
fn moves_focus_forward_and_wraps() {
    let mut m = menu(2);
    let mut input = Input::new(W, H, 0);
    let mut seen = Vec::new();
    for _ in 0..5 {
        assert_eq!(input.handle(&mut m, Event::Next), Action::Redraw);
        seen.push(focus(&m).unwrap());
    }
    assert_eq!(
        seen,
        [
            Item::Card(1),
            Item::Button(Power::Restart),
            Item::Button(Power::ShutDown),
            Item::Card(0),
            Item::Card(1),
        ]
    );
}

#[test]
fn moves_focus_backward_and_wraps() {
    let mut m = menu(2);
    let mut input = Input::new(W, H, 0);
    input.handle(&mut m, Event::Previous);
    assert_eq!(focus(&m), Some(Item::Button(Power::ShutDown)));
    assert_eq!(m.focus.unwrap().1, FocusSource::Keyboard);
}

#[test]
fn starts_focus_at_either_end_when_nothing_is_focused() {
    let mut m = menu(0);
    let mut input = Input::new(W, H, 0);
    input.handle(&mut m, Event::Next);
    assert_eq!(focus(&m), Some(Item::Button(Power::Restart)));
    let mut m = menu(0);
    input.handle(&mut m, Event::Previous);
    assert_eq!(focus(&m), Some(Item::Button(Power::ShutDown)));
}

#[test]
fn activates_the_focused_item() {
    let mut m = menu(2);
    let mut input = Input::new(W, H, 0);
    assert_eq!(
        input.handle(&mut m, Event::Activate),
        Action::Activate(Item::Card(0))
    );
    let mut empty = menu(0);
    assert_eq!(input.handle(&mut empty, Event::Activate), Action::None);
}

#[test]
fn ignores_other_keys() {
    let mut m = menu(2);
    let mut input = Input::new(W, H, 0);
    assert_eq!(input.handle(&mut m, Event::OtherKey), Action::None);
    assert_eq!(focus(&m), Some(Item::Card(0)));
}

#[test]
fn counts_down_and_starts_the_entry() {
    let mut m = menu(3);
    m.countdown = Some(("Entry 2".to_string(), 2));
    let mut input = Input::new(W, H, 2);
    assert_eq!(input.handle(&mut m, Event::Second), Action::Redraw);
    assert_eq!(m.countdown.as_ref().unwrap().1, 1);
    assert_eq!(
        input.handle(&mut m, Event::Second),
        Action::Activate(Item::Card(2))
    );
    assert_eq!(m.countdown, None);
    assert_eq!(input.handle(&mut m, Event::Second), Action::None);
}

#[test]
fn any_input_stops_the_countdown() {
    let events = [
        Event::OtherKey,
        Event::Next,
        Event::PointerMoved(0, 0),
        Event::PointerPressed,
    ];
    for event in events {
        let mut m = menu(1);
        m.countdown = Some(("Entry 0".to_string(), 5));
        let mut input = Input::new(W, H, 0);
        assert_eq!(input.handle(&mut m, event), Action::Redraw, "{event:?}");
        assert_eq!(m.countdown, None, "{event:?}");
    }
}

#[test]
fn pointer_focuses_items_it_moves_over() {
    let mut m = menu(3);
    let mut input = Input::new(W, H, 0);
    // The middle card of three is centered on the screen.
    input.handle(&mut m, Event::PointerMoved(960, 500));
    assert_eq!(m.focus, Some((Item::Card(1), FocusSource::Pointer)));
    input.handle(&mut m, Event::PointerMoved(10, 10));
    assert_eq!(m.focus, Some((Item::Card(1), FocusSource::Pointer)));
    assert_eq!(input.pointer(), Some((10, 10)));
}

#[test]
fn clicks_activate_only_when_pressed_and_released_on_one_item() {
    let mut m = menu(3);
    let mut input = Input::new(W, H, 0);
    input.handle(&mut m, Event::PointerMoved(960, 500));
    input.handle(&mut m, Event::PointerPressed);
    assert_eq!(
        input.handle(&mut m, Event::PointerReleased),
        Action::Activate(Item::Card(1))
    );

    input.handle(&mut m, Event::PointerPressed);
    input.handle(&mut m, Event::PointerMoved(1148, 500));
    assert_eq!(input.handle(&mut m, Event::PointerReleased), Action::None);

    input.handle(&mut m, Event::PointerMoved(10, 10));
    input.handle(&mut m, Event::PointerPressed);
    assert_eq!(input.handle(&mut m, Event::PointerReleased), Action::None);
}

#[test]
fn moves_between_rows_to_the_nearest_card() {
    // On a 640-pixel screen three cards fit in a row: rows 0–2, 3–5, 6.
    let (w, h) = (640, 2000);
    let mut m = menu(7);
    let mut input = Input::new(w, h, 0);
    assert_eq!(m.grid(w, h).per_row, 3);
    input.handle(&mut m, Event::Up);
    assert_eq!(focus(&m), Some(Item::Card(0)), "up from the first row");
    m.focus = Some((Item::Card(2), FocusSource::Keyboard));
    input.handle(&mut m, Event::Down);
    assert_eq!(focus(&m), Some(Item::Card(5)));
    input.handle(&mut m, Event::Down);
    assert_eq!(focus(&m), Some(Item::Card(6)), "the last row's one card");
    input.handle(&mut m, Event::Down);
    assert_eq!(focus(&m), Some(Item::Button(Power::Restart)));
    assert_eq!(input.handle(&mut m, Event::Down), Action::None);
    assert_eq!(focus(&m), Some(Item::Button(Power::Restart)));
    m.focus = Some((Item::Button(Power::ShutDown), FocusSource::Keyboard));
    input.handle(&mut m, Event::Up);
    assert_eq!(focus(&m), Some(Item::Card(6)));
    input.handle(&mut m, Event::Up);
    assert_eq!(focus(&m), Some(Item::Card(4)), "the middle card above");
    // Right and Left still walk the focus order across rows.
    input.handle(&mut m, Event::Previous);
    assert_eq!(focus(&m), Some(Item::Card(3)));
    input.handle(&mut m, Event::Previous);
    assert_eq!(focus(&m), Some(Item::Card(2)));
}

#[test]
fn stays_on_a_button_without_cards() {
    let mut m = menu(0);
    m.focus = Some((Item::Button(Power::Restart), FocusSource::Keyboard));
    let mut input = Input::new(W, H, 0);
    assert_eq!(input.handle(&mut m, Event::Up), Action::None);
    assert_eq!(focus(&m), Some(Item::Button(Power::Restart)));
}
