//! Areas of a surface as rectangles that do not overlap, which the
//! rectangles that a `wl_region` adds and takes away, in order, make, and
//! from which other areas can be cut.

use super::Rect;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Area(Vec<Rect>);

impl Area {
    /// The area of a region's rectangles, each added or, when false,
    /// taken away, in order.
    pub fn of(region: &[(bool, Rect)]) -> Area {
        let mut area = Area::default();
        for &(add, rect) in region {
            match add {
                true => area.add(rect),
                false => area.subtract(rect),
            }
        }
        area
    }

    pub fn rects(&self) -> &[Rect] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn add(&mut self, rect: Rect) {
        if rect.width > 0 && rect.height > 0 {
            self.subtract(rect);
            self.0.push(rect);
        }
    }

    pub fn subtract(&mut self, cut: Rect) {
        if cut.width <= 0 || cut.height <= 0 {
            return;
        }
        let rects = std::mem::take(&mut self.0);
        self.0 = rects.into_iter().flat_map(|r| outside(r, cut)).collect();
    }

    /// Takes `other` away from this area.
    pub fn cut(&mut self, other: &Area) {
        for &rect in &other.0 {
            self.subtract(rect);
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.0
            .iter()
            .any(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
    }
}

/// The parts of `rect` outside `cut`: up to four rectangles, the rows
/// above and below `cut` across the whole width, and the parts left and
/// right of it between them.
fn outside(rect: Rect, cut: Rect) -> Vec<Rect> {
    let (x0, y0) = (rect.x.max(cut.x), rect.y.max(cut.y));
    let x1 = (rect.x + rect.width).min(cut.x + cut.width);
    let y1 = (rect.y + rect.height).min(cut.y + cut.height);
    if x1 <= x0 || y1 <= y0 {
        return vec![rect];
    }
    let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
    let parts = [
        (rect.x, rect.y, right, y0),
        (rect.x, y1, right, bottom),
        (rect.x, y0, x0, y1),
        (x1, y0, right, y1),
    ];
    parts
        .into_iter()
        .filter(|&(l, t, r, b)| r > l && b > t)
        .map(|(l, t, r, b)| Rect {
            x: l,
            y: t,
            width: r - l,
            height: b - t,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The number of the area's pixels within 0..100 by 0..100.
    fn size(area: &Area) -> usize {
        (0..100)
            .flat_map(|y| (0..100).map(move |x| (x, y)))
            .filter(|&(x, y)| area.contains(x, y))
            .count()
    }

    #[test]
    fn adds_and_takes_away_in_order() {
        let area = Area::of(&[
            (true, rect(0, 0, 10, 10)),
            (false, rect(2, 2, 4, 4)),
            (true, rect(5, 5, 10, 10)),
        ]);
        // The second square overlaps 25 pixels of the first, one of them in
        // the hole.
        assert_eq!(size(&area), 100 - 16 + 100 - 24);
        assert!(!area.contains(3, 3));
        assert!(area.contains(5, 5));
        assert!(area.contains(14, 14));
        let total: i32 = area.rects().iter().map(|r| r.width * r.height).sum();
        assert_eq!(total as usize, size(&area));
    }

    #[test]
    fn cuts_one_area_out_of_another() {
        let mut haze = Area::of(&[(true, rect(0, 0, 50, 50))]);
        haze.cut(&Area::of(&[(true, rect(10, 0, 50, 10))]));
        assert_eq!(size(&haze), 2500 - 400);
        assert!(haze.contains(5, 5));
        assert!(!haze.contains(15, 5));
        haze.cut(&Area::of(&[(true, rect(0, 0, 100, 100))]));
        assert!(haze.is_empty());
    }

    #[test]
    fn ignores_empty_rectangles() {
        let area = Area::of(&[(true, rect(0, 0, 0, 10)), (true, rect(0, 0, 4, -1))]);
        assert!(area.is_empty());
    }
}
