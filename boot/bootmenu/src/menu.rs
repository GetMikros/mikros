//! The state of the boot menu and how it is laid out and drawn.
//!
//! Positions are fixed offsets in pixels from the center of the screen, or
//! from its top or bottom edge, so the menu keeps the same size at every
//! resolution.

use crate::canvas::{Canvas, Color};
use crate::shape;
use crate::text::{Style, Text, Weight};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const BACKGROUND: Color = Color(0x000000);
const PRIMARY: Color = Color(0xf2f2f3);
const SECONDARY: Color = Color(0x8b8f96);
const ACCENT: Color = Color(0x3b82f6);
const WARNING: Color = Color(0xfbbf24);
const SUBTLE_OUTLINE: Color = Color(0x3a3d43);
const SURFACE: Color = Color(0x111214);
const SURFACE_FOCUSED: Color = Color(0x1b1d20);
const BORDER: Color = Color(0x232529);
const ICON: Color = Color(0xb5b8be);
const ICON_FOCUSED: Color = Color(0xffffff);
const TILE_TEXT: Color = Color(0xffffff);

const HEADING: Style = Style {
    weight: Weight::SemiBold,
    size: 19.835,
};
const TILE_LETTER: Style = Style {
    weight: Weight::SemiBold,
    size: 26.447,
};
const NAME: Style = Style {
    weight: Weight::SemiBold,
    size: 13.224,
};
const BODY: Style = Style {
    weight: Weight::Regular,
    size: 13.224,
};
const SMALL: Style = Style {
    weight: Weight::Regular,
    size: 11.571,
};

const CARD_SIZE: i32 = 162;
const CARD_SPACING: i32 = 188;
const CARD_TOP: i32 = -121;
const BUTTON_RADIUS: f32 = 28.0;
const BUTTON_SPACING: i32 = 88;
const BUTTON_Y: i32 = 134;

/// A power action offered by a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
    FirmwareSettings,
}

impl Power {
    pub fn label(self) -> &'static str {
        match self {
            Power::Restart => "Restart",
            Power::ShutDown => "Shut down",
            Power::FirmwareSettings => "Firmware settings",
        }
    }
}

/// An item that can have focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Card(usize),
    Button(Power),
}

/// How the focused item got its focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusSource {
    Keyboard,
    Pointer,
}

/// What a card shows.
pub struct Card {
    pub name: String,
    pub detail: Option<String>,
    pub color: Color,
}

/// Everything that the menu shows.
pub struct Menu {
    pub cards: Vec<Card>,
    pub buttons: Vec<Power>,
    pub notice: Option<String>,
    pub focus: Option<(Item, FocusSource)>,
    /// The name of the entry and the seconds left while a countdown runs.
    pub countdown: Option<(String, u32)>,
}

/// A rectangle as (x, y, width, height).
pub type Rect = (i32, i32, i32, i32);

impl Menu {
    /// The items in focus order.
    pub fn items(&self) -> Vec<Item> {
        let cards = (0..self.cards.len()).map(Item::Card);
        cards
            .chain(self.buttons.iter().copied().map(Item::Button))
            .collect()
    }

    /// The area of `item` on a screen of `width` × `height` pixels.
    pub fn bounds(&self, item: Item, width: i32, height: i32) -> Rect {
        let (cx, cy) = (width / 2, height / 2);
        match item {
            Item::Card(index) => {
                let x = card_center(cx, index, self.cards.len()) - CARD_SIZE / 2;
                (x, cy + CARD_TOP, CARD_SIZE, CARD_SIZE)
            }
            Item::Button(power) => {
                let index = self.buttons.iter().position(|&b| b == power).unwrap_or(0);
                let x = row_center(cx, index, self.buttons.len(), BUTTON_SPACING);
                let r = BUTTON_RADIUS as i32;
                (x - r, cy + BUTTON_Y - r, 2 * r, 2 * r)
            }
        }
    }

    /// The item at (`x`, `y`), if any.
    pub fn item_at(&self, x: i32, y: i32, width: i32, height: i32) -> Option<Item> {
        self.items().into_iter().find(|&item| {
            let (ix, iy, iw, ih) = self.bounds(item, width, height);
            (ix..ix + iw).contains(&x) && (iy..iy + ih).contains(&y)
        })
    }

    pub fn draw(&self, text: &mut Text, canvas: &mut Canvas) {
        let (width, height) = (canvas.width() as i32, canvas.height() as i32);
        let (cx, cy) = (width / 2, height / 2);
        canvas.clear(BACKGROUND);
        text.draw_centered(
            canvas,
            "Choose a system to boot",
            HEADING,
            cx,
            cy - 167,
            PRIMARY,
        );

        if self.cards.is_empty() {
            self.draw_empty(text, canvas);
        } else {
            for index in 0..self.cards.len() {
                self.draw_card(text, canvas, index);
            }
            if let Some(notice) = &self.notice {
                text.draw_centered(canvas, notice, BODY, cx, 53, WARNING);
            }
        }

        for &power in &self.buttons {
            self.draw_button(text, canvas, power);
        }

        match &self.countdown {
            Some((name, seconds)) => {
                let line = format!("Booting {name} in {seconds} s. Press any key to stay.");
                text.draw_centered(canvas, &line, BODY, cx, height - 19, PRIMARY);
            }
            None => {
                let hint = "Arrow keys to choose, Enter to boot";
                text.draw_centered(canvas, hint, SMALL, cx, height - 20, SECONDARY);
            }
        }
    }

    fn focus_of(&self, item: Item) -> Option<FocusSource> {
        self.focus
            .filter(|&(i, _)| i == item)
            .map(|(_, source)| source)
    }

    fn draw_card(&self, text: &mut Text, canvas: &mut Canvas, index: usize) {
        let card = &self.cards[index];
        let (x, y, w, h) = self.bounds(
            Item::Card(index),
            canvas.width() as i32,
            canvas.height() as i32,
        );
        let center = x + w / 2;
        let rect = |inset: f32| {
            (
                x as f32 - inset,
                y as f32 - inset,
                w as f32 + 2.0 * inset,
                h as f32 + 2.0 * inset,
            )
        };

        let focus = self.focus_of(Item::Card(index));
        match focus {
            Some(FocusSource::Keyboard) => {
                shape::stroke_rounded_rect(canvas, rect(0.0), 13.0, 2.0, ACCENT)
            }
            Some(FocusSource::Pointer) => {
                shape::stroke_rounded_rect(canvas, rect(0.5), 13.5, 1.0, SUBTLE_OUTLINE)
            }
            None => {}
        }

        let tile = ((center - 36) as f32, (y + 22) as f32, 72.0, 72.0);
        shape::fill_rounded_rect(canvas, tile, 15.0, card.color);
        let mut letter = [0; 4];
        let letter = card
            .name
            .chars()
            .next()
            .map_or("", |c| c.encode_utf8(&mut letter));
        text.draw_centered(canvas, letter, TILE_LETTER, center, y + 68, TILE_TEXT);

        text.draw_centered(canvas, &card.name, NAME, center, y + 119, PRIMARY);
        if let Some(detail) = &card.detail {
            text.draw_centered(canvas, detail, SMALL, center, y + 136, SECONDARY);
        }
        if focus.is_some() {
            text.draw_centered(canvas, "Press to boot", SMALL, center, y + 184, ACCENT);
        }
    }

    fn draw_empty(&self, text: &mut Text, canvas: &mut Canvas) {
        let (cx, cy) = (canvas.width() as i32 / 2, canvas.height() as i32 / 2);
        let message = self
            .notice
            .as_deref()
            .unwrap_or("Add boot entries to \\EFI\\Mikros\\entries.yaml");
        let width = (text.width(message, BODY) + 80).max(559) as f32;
        let rect = (
            cx as f32 - width / 2.0,
            (cy - 110) as f32 + 0.5,
            width,
            163.0,
        );
        shape::fill_rounded_rect(canvas, rect, 13.5, SURFACE);
        shape::stroke_rounded_rect(canvas, rect, 13.5, 1.0, BORDER);
        text.draw_centered(canvas, "No boot entries", NAME, cx, cy - 36, PRIMARY);
        text.draw_centered(canvas, message, BODY, cx, cy - 10, SECONDARY);
    }

    fn draw_button(&self, text: &mut Text, canvas: &mut Canvas, power: Power) {
        let (x, y, w, h) = self.bounds(
            Item::Button(power),
            canvas.width() as i32,
            canvas.height() as i32,
        );
        let center = ((x + w / 2) as f32, (y + h / 2) as f32);
        let focus = self.focus_of(Item::Button(power));
        let (fill, icon) = match focus {
            Some(FocusSource::Keyboard) => (SURFACE_FOCUSED, ICON_FOCUSED),
            _ => (SURFACE, ICON),
        };
        shape::fill_circle(canvas, center, BUTTON_RADIUS, fill);
        match focus {
            Some(FocusSource::Keyboard) => shape::stroke_circle(canvas, center, 27.0, 2.0, ACCENT),
            Some(FocusSource::Pointer) => {
                shape::stroke_circle(canvas, center, 27.5, 1.0, SUBTLE_OUTLINE)
            }
            None => shape::stroke_circle(canvas, center, 27.5, 1.0, BORDER),
        }
        draw_power_icon(canvas, power, center, icon, fill);
        if focus.is_some() {
            let label_y = canvas.height() as i32 / 2 + BUTTON_Y + 49;
            text.draw_centered(
                canvas,
                power.label(),
                SMALL,
                canvas.width() as i32 / 2,
                label_y,
                SECONDARY,
            );
        }
    }
}

/// The x of the center of item `index` of `count` items in a row centered on
/// `cx`, `spacing` pixels apart.
fn row_center(cx: i32, index: usize, count: usize, spacing: i32) -> i32 {
    cx + (2 * index as i32 - (count as i32 - 1)) * spacing / 2
}

fn card_center(cx: i32, index: usize, count: usize) -> i32 {
    row_center(cx, index, count, CARD_SPACING)
}

/// The corners of the gear of the firmware settings icon, relative to the
/// button center.
const GEAR: [(f32, f32); 32] = [
    (7.27, -1.75),
    (10.03, -1.35),
    (10.03, 1.35),
    (7.27, 1.75),
    (6.38, 3.91),
    (8.04, 6.14),
    (6.14, 8.04),
    (3.91, 6.38),
    (1.75, 7.27),
    (1.35, 10.03),
    (-1.35, 10.03),
    (-1.75, 7.27),
    (-3.91, 6.38),
    (-6.14, 8.04),
    (-8.04, 6.14),
    (-6.38, 3.91),
    (-7.27, 1.75),
    (-10.03, 1.35),
    (-10.03, -1.35),
    (-7.27, -1.75),
    (-6.38, -3.91),
    (-8.04, -6.14),
    (-6.14, -8.04),
    (-3.91, -6.38),
    (-1.75, -7.27),
    (-1.35, -10.03),
    (1.35, -10.03),
    (1.75, -7.27),
    (3.91, -6.38),
    (6.14, -8.04),
    (8.04, -6.14),
    (6.38, -3.91),
];

/// Draws the icon of `power` centered on `center`. `fill` is the color of
/// the button behind it, used for the hole of the gear.
fn draw_power_icon(
    canvas: &mut Canvas,
    power: Power,
    (cx, cy): (f32, f32),
    color: Color,
    fill: Color,
) {
    let at = |dx: f32, dy: f32| (cx + dx, cy + dy);
    const STROKE: f32 = 1.98;
    match power {
        Power::Restart => {
            shape::stroke_arc(
                canvas,
                (cx, cy),
                at(5.86, -4.65),
                at(-2.56, -7.03),
                STROKE,
                color,
            );
            let head = [at(1.57, -8.54), at(-3.85, -10.54), at(-1.28, -3.51)];
            shape::fill_polygon(canvas, &head, color);
        }
        Power::ShutDown => {
            shape::stroke_arc(
                canvas,
                (cx, cy),
                at(5.40, -5.79),
                at(-5.40, -5.79),
                STROKE,
                color,
            );
            shape::stroke_line(canvas, at(0.0, -9.68), at(0.0, -1.32), STROKE, color);
        }
        Power::FirmwareSettings => {
            let gear: Vec<(f32, f32)> = GEAR.iter().map(|&(dx, dy)| at(dx, dy)).collect();
            shape::fill_polygon(canvas, &gear, color);
            shape::fill_circle(canvas, (cx, cy), 3.30, fill);
        }
    }
}

#[cfg(test)]
mod tests;
