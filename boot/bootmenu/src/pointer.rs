//! Turning pointer device states into menu events, and drawing the pointer.

use crate::canvas::{Canvas, Color};
use crate::input::Event;
use crate::shape;
use alloc::vec::Vec;

/// Pixels the pointer moves per millimeter of relative device movement.
const PIXELS_PER_MM: i64 = 8;

/// The pointer position and primary button state, in screen pixels.
pub struct Tracker {
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    pressed: bool,
}

impl Tracker {
    /// Starts with the pointer in the middle of a `width` × `height` screen.
    pub fn new(width: i32, height: i32) -> Tracker {
        Tracker {
            width,
            height,
            x: width / 2,
            y: height / 2,
            pressed: false,
        }
    }

    /// Applies a relative movement of (`dx`, `dy`) device counts, with the
    /// device's resolution in counts per millimeter (0 when unknown).
    pub fn relative(
        &mut self,
        dx: i32,
        dy: i32,
        resolution: (u64, u64),
        pressed: bool,
    ) -> Vec<Event> {
        let scale = |delta: i32, resolution: u64| -> i32 {
            let delta = i64::from(delta);
            match resolution {
                0 => delta as i32,
                r => (delta * PIXELS_PER_MM / r.min(i64::MAX as u64) as i64) as i32,
            }
        };
        let x = self.x.saturating_add(scale(dx, resolution.0));
        let y = self.y.saturating_add(scale(dy, resolution.1));
        self.update(x, y, pressed)
    }

    /// Applies an absolute position `(x, y)` within the device ranges
    /// `x_range` and `y_range`, both inclusive.
    pub fn absolute(
        &mut self,
        (x, y): (u64, u64),
        x_range: (u64, u64),
        y_range: (u64, u64),
        pressed: bool,
    ) -> Vec<Event> {
        let map = |value: u64, (min, max): (u64, u64), size: i32| -> Option<i32> {
            if max <= min || size <= 0 {
                return None;
            }
            let offset = u128::from(value.clamp(min, max) - min);
            Some((offset * (size as u128 - 1) / u128::from(max - min)) as i32)
        };
        let x = map(x, x_range, self.width).unwrap_or(self.x);
        let y = map(y, y_range, self.height).unwrap_or(self.y);
        self.update(x, y, pressed)
    }

    fn update(&mut self, x: i32, y: i32, pressed: bool) -> Vec<Event> {
        let mut events = Vec::new();
        let x = x.clamp(0, self.width - 1);
        let y = y.clamp(0, self.height - 1);
        if (x, y) != (self.x, self.y) {
            (self.x, self.y) = (x, y);
            events.push(Event::PointerMoved(x, y));
        }
        if pressed != self.pressed {
            self.pressed = pressed;
            events.push(if pressed {
                Event::PointerPressed
            } else {
                Event::PointerReleased
            });
        }
        events
    }
}

/// The corners of the arrow, relative to its tip.
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 11.52),
    (2.88, 8.86),
    (4.75, 13.18),
    (6.62, 12.38),
    (4.75, 8.14),
    (8.35, 8.14),
];

/// Draws the pointer with its tip on pixel (`x`, `y`).
pub fn draw(canvas: &mut Canvas, x: i32, y: i32) {
    let (tx, ty) = (x as f32 + 0.5, y as f32 + 0.5);
    let points: Vec<(f32, f32)> = ARROW.iter().map(|&(dx, dy)| (tx + dx, ty + dy)).collect();
    let white = Color(0xffffff);
    for (i, &a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        shape::stroke_line(canvas, a, b, 2.3, white);
    }
    shape::fill_polygon(canvas, &points, Color(0x000000));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_relatively_and_clamps() {
        let mut t = Tracker::new(100, 80);
        assert_eq!(
            t.relative(5, -3, (0, 0), false),
            [Event::PointerMoved(55, 37)]
        );
        assert_eq!(
            t.relative(16, 8, (8, 8), false),
            [Event::PointerMoved(71, 45)]
        );
        assert_eq!(
            t.relative(1000, 1000, (0, 0), false),
            [Event::PointerMoved(99, 79)]
        );
        assert_eq!(t.relative(0, 0, (0, 0), false), []);
    }

    #[test]
    fn maps_absolute_positions_to_the_screen() {
        let mut t = Tracker::new(101, 51);
        assert_eq!(
            t.absolute((0, 0), (0, 1000), (0, 1000), false),
            [Event::PointerMoved(0, 0)]
        );
        assert_eq!(
            t.absolute((500, 1000), (0, 1000), (0, 1000), false),
            [Event::PointerMoved(50, 50)]
        );
        assert_eq!(
            t.absolute((2000, 7), (0, 1000), (5, 5), false),
            [Event::PointerMoved(100, 50)]
        );
    }

    #[test]
    fn reports_button_changes() {
        let mut t = Tracker::new(100, 80);
        assert_eq!(t.relative(0, 0, (0, 0), true), [Event::PointerPressed]);
        assert_eq!(t.relative(0, 0, (0, 0), true), []);
        assert_eq!(
            t.relative(1, 0, (0, 0), false),
            [Event::PointerMoved(51, 40), Event::PointerReleased]
        );
    }

    #[test]
    fn draws_a_white_outlined_black_arrow() {
        let mut canvas = Canvas::new(30, 30);
        canvas.clear(Color(0x202020));
        draw(&mut canvas, 10, 10);
        assert!(
            canvas.get(9, 14).r() > 0x90,
            "white outline beside the left edge"
        );
        assert_eq!(canvas.get(12, 17), Color(0x000000), "black inside");
        assert_eq!(canvas.get(25, 25), Color(0x202020), "nothing far away");
    }
}
