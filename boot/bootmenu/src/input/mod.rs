//! How input changes the menu.

use crate::menu::{FocusSource, Item, Menu};

/// An input event, already translated from the firmware's representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Tab or Right.
    Next,
    /// Left.
    Previous,
    /// Down: to the next row of cards, or from the last to the buttons.
    Down,
    /// Up: to the previous row of cards, or from the buttons to the last.
    Up,
    /// Enter or Space.
    Activate,
    /// Any other key.
    OtherKey,
    /// The pointer moved to (x, y) on the screen.
    PointerMoved(i32, i32),
    /// The primary button went down.
    PointerPressed,
    /// The primary button went up.
    PointerReleased,
    /// One second of the countdown passed.
    Second,
}

/// A key stroke as the console reports it, without the firmware's types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Right,
    Left,
    /// A key with a character.
    Char(char),
    /// Any other key without one.
    OtherSpecial,
}

/// The menu event of `key`.
pub fn translate(key: Key) -> Event {
    match key {
        Key::Right | Key::Char('\t') => Event::Next,
        Key::Left => Event::Previous,
        Key::Down => Event::Down,
        Key::Up => Event::Up,
        Key::Char('\r' | '\n' | ' ') => Event::Activate,
        Key::Char(_) | Key::OtherSpecial => Event::OtherKey,
    }
}

/// What the caller has to do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing changed.
    None,
    /// The menu changed and must be drawn again.
    Redraw,
    /// The item was activated.
    Activate(Item),
}

/// The input state that is not shown in the menu.
pub struct Input {
    /// The size of the screen, for finding items under the pointer.
    width: i32,
    height: i32,
    pointer: Option<(i32, i32)>,
    /// The item the primary button went down on.
    pressed: Option<Item>,
    /// The entry the countdown starts.
    countdown_entry: usize,
}

impl Input {
    pub fn new(width: i32, height: i32, countdown_entry: usize) -> Input {
        Input {
            width,
            height,
            pointer: None,
            pressed: None,
            countdown_entry,
        }
    }

    /// Where the pointer is, once it has moved.
    pub fn pointer(&self) -> Option<(i32, i32)> {
        self.pointer
    }

    /// Applies `event` to `menu`.
    pub fn handle(&mut self, menu: &mut Menu, event: Event) -> Action {
        if event == Event::Second {
            return self.tick(menu);
        }
        let stopped = menu.countdown.take().is_some();
        let action = match event {
            Event::Next => move_focus(menu, 1),
            Event::Previous => move_focus(menu, -1),
            Event::Down => move_vertically(menu, true, self.width, self.height),
            Event::Up => move_vertically(menu, false, self.width, self.height),
            Event::Activate => match menu.focus {
                Some((item, _)) => Action::Activate(item),
                None => Action::None,
            },
            Event::OtherKey | Event::Second => Action::None,
            Event::PointerMoved(x, y) => {
                self.pointer = Some((x, y));
                if let Some(item) = menu.item_at(x, y, self.width, self.height) {
                    menu.focus = Some((item, FocusSource::Pointer));
                }
                Action::Redraw
            }
            Event::PointerPressed => {
                self.pressed = self.item_under_pointer(menu);
                Action::None
            }
            Event::PointerReleased => {
                let pressed = self.pressed.take();
                match pressed {
                    Some(item) if self.item_under_pointer(menu) == Some(item) => {
                        Action::Activate(item)
                    }
                    _ => Action::None,
                }
            }
        };
        match action {
            Action::None if stopped => Action::Redraw,
            action => action,
        }
    }

    fn item_under_pointer(&self, menu: &Menu) -> Option<Item> {
        let (x, y) = self.pointer?;
        menu.item_at(x, y, self.width, self.height)
    }

    fn tick(&mut self, menu: &mut Menu) -> Action {
        match &mut menu.countdown {
            Some((_, seconds)) if *seconds > 1 => {
                *seconds -= 1;
                Action::Redraw
            }
            Some(_) => {
                menu.countdown = None;
                Action::Activate(Item::Card(self.countdown_entry))
            }
            None => Action::None,
        }
    }
}

/// Moves the keyboard focus `step` items along the focus order, wrapping
/// around at both ends.
fn move_focus(menu: &mut Menu, step: isize) -> Action {
    let items = menu.items();
    if items.is_empty() {
        return Action::None;
    }
    let count = items.len() as isize;
    let current = menu
        .focus
        .and_then(|(item, _)| items.iter().position(|&i| i == item));
    let next = match current {
        Some(index) => (index as isize + step).rem_euclid(count),
        None if step > 0 => 0,
        None => count - 1,
    };
    menu.focus = Some((items[next as usize], FocusSource::Keyboard));
    Action::Redraw
}

/// Moves the keyboard focus a row down or up on a screen of `width` ×
/// `height` pixels: from a card to the card of the next or previous row
/// whose center is horizontally nearest, from the last row down to the
/// first button, and from a button up to the card of the last row nearest
/// to it. Up from the first row and down from a button change nothing;
/// without a focus, the first or the last item gets it.
fn move_vertically(menu: &mut Menu, down: bool, width: i32, height: i32) -> Action {
    let Some((item, _)) = menu.focus else {
        return move_focus(menu, if down { 1 } else { -1 });
    };
    let grid = menu.grid(width, height);
    let center = |item| {
        let (x, _, w, _) = menu.bounds(item, width, height);
        x + w / 2
    };
    let nearest = |row: usize, x: i32| {
        let first = row * grid.per_row;
        let last = (first + grid.per_row).min(menu.cards.len());
        (first..last).min_by_key(|&i| (center(Item::Card(i)) - x).abs())
    };
    let target = match item {
        Item::Card(index) => {
            let row = Menu::row(&grid, index);
            let x = center(item);
            match down {
                true if row + 1 < grid.rows => nearest(row + 1, x).map(Item::Card),
                true => menu.buttons.first().copied().map(Item::Button),
                false if row > 0 => nearest(row - 1, x).map(Item::Card),
                false => None,
            }
        }
        Item::Button(_) if down => None,
        Item::Button(_) if menu.cards.is_empty() => None,
        Item::Button(_) => nearest(grid.rows - 1, center(item)).map(Item::Card),
    };
    match target {
        Some(target) => {
            menu.focus = Some((target, FocusSource::Keyboard));
            Action::Redraw
        }
        None => Action::None,
    }
}

#[cfg(test)]
mod tests;
