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
