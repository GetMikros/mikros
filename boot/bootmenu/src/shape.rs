//! Anti-aliased shapes drawn into a [`Canvas`].
//!
//! Coordinates are in pixels, with pixel (x, y) covering the square from
//! (x, y) to (x + 1, y + 1). Outlines are centered on the shape's edge.
//! Most shapes are drawn from a signed distance: the coverage of a pixel is
//! taken from the distance between its center and the edge.

use crate::canvas::{Canvas, Color};
use libm::{atan2f, sqrtf};

const TAU: f32 = core::f32::consts::TAU;

/// The coverage of a pixel whose center is `distance` pixels outside the
/// shape (negative inside).
fn coverage(distance: f32) -> u8 {
    let c = (0.5 - distance).clamp(0.0, 1.0);
    (c * 255.0 + 0.5) as u8
}

/// Blends `color` into every pixel of the box from (`x0`, `y0`) to
/// (`x1`, `y1`), using `distance` for each pixel center.
fn fill_distance(
    canvas: &mut Canvas,
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    color: Color,
    distance: impl Fn(f32, f32) -> f32,
) {
    let floor = |v: f32| libm::floorf(v) as i32;
    let ceil = |v: f32| libm::ceilf(v) as i32;
    let left = floor(x0 - 1.0).max(0);
    let top = floor(y0 - 1.0).max(0);
    let right = ceil(x1 + 1.0).min(canvas.width() as i32);
    let bottom = ceil(y1 + 1.0).min(canvas.height() as i32);
    for y in top..bottom {
        for x in left..right {
            let d = distance(x as f32 + 0.5, y as f32 + 0.5);
            canvas.blend(x, y, color, coverage(d));
        }
    }
}

/// The signed distance from (`px`, `py`) to a rectangle with corner radius
/// `r`.
fn rounded_rect_distance(px: f32, py: f32, (x, y, w, h): (f32, f32, f32, f32), r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0);
    let qx = (px - (x + w / 2.0)).abs() - (w / 2.0 - r);
    let qy = (py - (y + h / 2.0)).abs() - (h / 2.0 - r);
    let outside = sqrtf(qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0));
    outside + qx.max(qy).min(0.0) - r
}

pub fn fill_rounded_rect(canvas: &mut Canvas, rect: (f32, f32, f32, f32), r: f32, color: Color) {
    let (x, y, w, h) = rect;
    fill_distance(canvas, (x, y, x + w, y + h), color, |px, py| {
        rounded_rect_distance(px, py, rect, r)
    });
}

pub fn stroke_rounded_rect(
    canvas: &mut Canvas,
    rect: (f32, f32, f32, f32),
    r: f32,
    width: f32,
    color: Color,
) {
    let (x, y, w, h) = rect;
    let half = width / 2.0;
    fill_distance(
        canvas,
        (x - half, y - half, x + w + half, y + h + half),
        color,
        |px, py| rounded_rect_distance(px, py, rect, r).abs() - half,
    );
}

fn length(dx: f32, dy: f32) -> f32 {
    sqrtf(dx * dx + dy * dy)
}

pub fn fill_circle(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: Color) {
    fill_distance(canvas, (cx - r, cy - r, cx + r, cy + r), color, |px, py| {
        length(px - cx, py - cy) - r
    });
}

pub fn stroke_circle(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, width: f32, color: Color) {
    let outer = r + width / 2.0;
    fill_distance(
        canvas,
        (cx - outer, cy - outer, cx + outer, cy + outer),
        color,
        |px, py| (length(px - cx, py - cy) - r).abs() - width / 2.0,
    );
}

/// The distance from (`px`, `py`) to the segment from `a` to `b`.
fn segment_distance(px: f32, py: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    length(px - (ax + t * dx), py - (ay + t * dy))
}

/// Draws a line from `a` to `b` with round caps.
pub fn stroke_line(canvas: &mut Canvas, a: (f32, f32), b: (f32, f32), width: f32, color: Color) {
    let half = width / 2.0;
    let bounds = (
        a.0.min(b.0) - half,
        a.1.min(b.1) - half,
        a.0.max(b.0) + half,
        a.1.max(b.1) + half,
    );
    fill_distance(canvas, bounds, color, |px, py| {
        segment_distance(px, py, a, b) - half
    });
}

/// The angle of (`x`, `y`) around the origin, increasing clockwise on the
/// screen, in `0..TAU`.
fn angle(x: f32, y: f32) -> f32 {
    let a = atan2f(y, x);
    if a < 0.0 { a + TAU } else { a }
}

/// Draws the arc of the circle around `center` through `start` and `end`,
/// going clockwise on the screen from `start` to `end`, with round caps.
pub fn stroke_arc(
    canvas: &mut Canvas,
    center: (f32, f32),
    start: (f32, f32),
    end: (f32, f32),
    width: f32,
    color: Color,
) {
    let (cx, cy) = center;
    let r = length(start.0 - cx, start.1 - cy);
    let from = angle(start.0 - cx, start.1 - cy);
    let mut sweep = angle(end.0 - cx, end.1 - cy) - from;
    if sweep < 0.0 {
        sweep += TAU;
    }
    let half = width / 2.0;
    let outer = r + half;
    fill_distance(
        canvas,
        (cx - outer, cy - outer, cx + outer, cy + outer),
        color,
        |px, py| {
            let mut along = angle(px - cx, py - cy) - from;
            if along < 0.0 {
                along += TAU;
            }
            if along <= sweep {
                (length(px - cx, py - cy) - r).abs() - half
            } else {
                let to_start = length(px - start.0, py - start.1);
                let to_end = length(px - end.0, py - end.1);
                to_start.min(to_end) - half
            }
        },
    );
}

/// Whether (`px`, `py`) is inside `points` under the even-odd rule.
fn inside(points: &[(f32, f32)], px: f32, py: f32) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > py) != (yj > py) && px < xi + (py - yi) * (xj - xi) / (yj - yi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Fills a polygon under the even-odd rule, with 4 × 4 samples per pixel.
pub fn fill_polygon(canvas: &mut Canvas, points: &[(f32, f32)], color: Color) {
    if points.len() < 3 {
        return;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in points {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let left = (libm::floorf(x0) as i32).max(0);
    let top = (libm::floorf(y0) as i32).max(0);
    let right = (libm::ceilf(x1) as i32).min(canvas.width() as i32);
    let bottom = (libm::ceilf(y1) as i32).min(canvas.height() as i32);
    for y in top..bottom {
        for x in left..right {
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    hits += u32::from(inside(points, px, py));
                }
            }
            canvas.blend(x, y, color, (hits * 255 / 16) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Color = Color::rgb(255, 255, 255);
    const BLACK: Color = Color(0);

    fn canvas() -> Canvas {
        Canvas::new(40, 40)
    }

    #[test]
    fn covers_pixels_by_distance() {
        assert_eq!(coverage(-1.0), 255);
        assert_eq!(coverage(0.0), 128);
        assert_eq!(coverage(1.0), 0);
    }

    #[test]
    fn fills_rounded_rectangles() {
        let mut c = canvas();
        fill_rounded_rect(&mut c, (10.0, 10.0, 20.0, 20.0), 6.0, WHITE);
        assert_eq!(c.get(20, 20), WHITE);
        assert_eq!(c.get(10, 20), WHITE);
        assert_eq!(c.get(10, 10), BLACK, "the corner is cut off");
        assert_eq!(c.get(9, 20), BLACK);
    }

    #[test]
    fn strokes_rounded_rectangles() {
        let mut c = canvas();
        stroke_rounded_rect(&mut c, (10.0, 10.0, 20.0, 20.0), 4.0, 2.0, WHITE);
        assert_eq!(c.get(20, 20), BLACK);
        assert_eq!(c.get(9, 20), WHITE);
        assert_eq!(c.get(10, 20), WHITE);
        assert_eq!(c.get(12, 20), BLACK);
    }

    #[test]
    fn fills_and_strokes_circles() {
        let mut c = canvas();
        fill_circle(&mut c, (20.0, 20.0), 10.0, WHITE);
        assert_eq!(c.get(20, 20), WHITE);
        assert_eq!(c.get(20, 31), BLACK);
        let mut c = canvas();
        stroke_circle(&mut c, (20.0, 20.0), 10.0, 2.0, WHITE);
        assert_eq!(c.get(20, 20), BLACK);
        assert_eq!(c.get(29, 19), WHITE);
    }

    #[test]
    fn strokes_lines_with_round_caps() {
        let mut c = canvas();
        stroke_line(&mut c, (10.0, 20.0), (30.0, 20.0), 4.0, WHITE);
        assert_eq!(c.get(20, 19), WHITE);
        assert_eq!(c.get(20, 23), BLACK);
        assert_eq!(c.get(9, 19), WHITE, "the cap reaches past the end");
        assert_eq!(c.get(6, 19), BLACK);
    }

    #[test]
    fn strokes_clockwise_arcs() {
        let mut c = canvas();
        // From the top of the circle clockwise to its right side.
        stroke_arc(&mut c, (20.0, 20.0), (20.0, 10.0), (30.0, 20.0), 2.0, WHITE);
        assert_eq!(c.get(26, 12), WHITE, "upper right quarter");
        assert_eq!(c.get(13, 26), BLACK, "lower left is not drawn");
        assert_eq!(c.get(12, 13), BLACK, "upper left is not drawn");
    }

    #[test]
    fn fills_polygons_even_odd() {
        let mut c = canvas();
        let square = [(10.0, 10.0), (30.0, 10.0), (30.0, 30.0), (10.0, 30.0)];
        fill_polygon(&mut c, &square, WHITE);
        assert_eq!(c.get(20, 20), WHITE);
        assert_eq!(c.get(31, 20), BLACK);
        let mut c = canvas();
        let half = [(10.0, 10.0), (10.5, 10.0), (10.5, 11.0), (10.0, 11.0)];
        fill_polygon(&mut c, &half, WHITE);
        assert_eq!(c.get(10, 10), Color::rgb(127, 127, 127));
    }
}
