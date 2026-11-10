//! Rectangles: which parts of a plane a commit copies.

use alloc::vec::Vec;

/// A rectangle with its top-left corner at `x`, `y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: u64,
    pub height: u64,
}

impl Rect {
    pub const fn new(x: i64, y: i64, width: u64, height: u64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn right(&self) -> i64 {
        self.x.saturating_add_unsigned(self.width)
    }

    fn bottom(&self) -> i64 {
        self.y.saturating_add_unsigned(self.height)
    }

    /// Whether `inner` lies entirely within this rectangle.
    pub fn contains(&self, inner: &Rect) -> bool {
        inner.x >= self.x
            && inner.y >= self.y
            && inner.right() <= self.right()
            && inner.bottom() <= self.bottom()
    }

    /// The part of `other` within this rectangle, if any.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > x && bottom > y).then(|| Rect::new(x, y, (right - x) as u64, (bottom - y) as u64))
    }
}

/// The parts of the plane's source rectangle `src` that a commit with
/// the damage rectangles `damage`, in buffer coordinates, copies: all of
/// `src` when there are none, else each damage rectangle within `src`.
pub fn copied(src: &Rect, damage: &[Rect]) -> Vec<Rect> {
    if damage.is_empty() {
        return alloc::vec![*src];
    }
    damage.iter().filter_map(|d| src.intersect(d)).collect()
}
