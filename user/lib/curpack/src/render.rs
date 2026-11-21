//! Rasterizing a frame of a cursor at a size in pixels with tiny-skia: the
//! theme's units are scaled to the size, each path is filled and then
//! stroked in order with anti-aliasing, and the result is premultiplied
//! BGRA, as the compositor draws it.

use crate::{Cap, Cursor, FillRule, Gradient, Join, Pack, Paint, Path, Spread, Verb};
use tiny_skia as ts;

/// A frame of a cursor, rasterized.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    /// The pixels across and down.
    pub size: u32,
    /// Premultiplied pixels, `0xAARRGGBB`, row by row.
    pub pixels: Vec<u32>,
    /// The hotspot in pixels.
    pub hotspot: (i32, i32),
}

impl Pack {
    /// Frame `frame` of `cursor`, `size` pixels across, or None for a
    /// frame that the cursor lacks or a size of 0.
    pub fn rasterize(&self, cursor: &Cursor, frame: usize, size: u32) -> Option<Image> {
        let frame = cursor.frames.get(frame)?;
        let mut pixmap = ts::Pixmap::new(size, size)?;
        let scale = size as f32 / f32::from(self.size);
        let transform = ts::Transform::from_scale(scale, scale);
        for path in &frame.paths {
            let Some(outline) = outline(path) else {
                continue;
            };
            if let Some(fill) = &path.fill
                && let Some(paint) = paint(&fill.paint)
            {
                let rule = match fill.rule {
                    FillRule::NonZero => ts::FillRule::Winding,
                    FillRule::EvenOdd => ts::FillRule::EvenOdd,
                };
                pixmap.fill_path(&outline, &paint, rule, transform, None);
            }
            if let Some(stroke) = &path.stroke
                && let Some(paint) = paint(&stroke.paint)
            {
                let line = ts::Stroke {
                    width: stroke.width,
                    miter_limit: stroke.miter_limit,
                    line_cap: match stroke.cap {
                        Cap::Butt => ts::LineCap::Butt,
                        Cap::Round => ts::LineCap::Round,
                        Cap::Square => ts::LineCap::Square,
                    },
                    line_join: match stroke.join {
                        Join::Miter => ts::LineJoin::Miter,
                        Join::Round => ts::LineJoin::Round,
                        Join::Bevel => ts::LineJoin::Bevel,
                    },
                    dash: None,
                };
                pixmap.stroke_path(&outline, &paint, &line, transform, None);
            }
        }
        let pixels = pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let [r, g, b, a] = p.map(u32::from);
                (a << 24) | (r << 16) | (g << 8) | b
            })
            .collect();
        let at = |v: f32| (v * scale).round() as i32;
        Some(Image {
            size,
            pixels,
            hotspot: (at(cursor.hotspot.0), at(cursor.hotspot.1)),
        })
    }
}

/// The path's outline, or None for one without area or length.
fn outline(path: &Path) -> Option<ts::Path> {
    let mut builder = ts::PathBuilder::new();
    let mut points = path.points.iter().copied();
    let mut next = || points.next().expect("a checked path has its points");
    for verb in &path.verbs {
        match verb {
            Verb::Move => {
                let (x, y) = next();
                builder.move_to(x, y);
            }
            Verb::Line => {
                let (x, y) = next();
                builder.line_to(x, y);
            }
            Verb::Quad => {
                let ((x1, y1), (x, y)) = (next(), next());
                builder.quad_to(x1, y1, x, y);
            }
            Verb::Cubic => {
                let ((x1, y1), (x2, y2), (x, y)) = (next(), next(), next());
                builder.cubic_to(x1, y1, x2, y2, x, y);
            }
            Verb::Close => builder.close(),
        }
    }
    builder.finish()
}

fn colour([r, g, b, a]: [u8; 4]) -> ts::Color {
    ts::Color::from_rgba8(r, g, b, a)
}

/// The paint of `paint`, or None for a gradient that paints nothing,
/// such as one whose start and end are the same point.
fn paint(paint: &Paint) -> Option<ts::Paint<'static>> {
    let shader = match paint {
        Paint::Solid(c) => ts::Shader::SolidColor(colour(*c)),
        Paint::Linear {
            start,
            end,
            gradient,
        } => {
            let (stops, spread, transform) = parts(gradient);
            let (start, end) = (point(*start), point(*end));
            ts::LinearGradient::new(start, end, stops, spread, transform)?
        }
        Paint::Radial {
            centre,
            radius,
            focal,
            gradient,
        } => {
            let (stops, spread, transform) = parts(gradient);
            let (focal, centre) = (point(*focal), point(*centre));
            ts::RadialGradient::new(focal, 0.0, centre, *radius, stops, spread, transform)?
        }
    };
    Some(ts::Paint {
        shader,
        anti_alias: true,
        ..ts::Paint::default()
    })
}

fn point((x, y): (f32, f32)) -> ts::Point {
    ts::Point::from_xy(x, y)
}

fn parts(gradient: &Gradient) -> (Vec<ts::GradientStop>, ts::SpreadMode, ts::Transform) {
    let stops = gradient
        .stops
        .iter()
        .map(|s| ts::GradientStop::new(s.offset, colour(s.colour)))
        .collect();
    let spread = match gradient.spread {
        Spread::Pad => ts::SpreadMode::Pad,
        Spread::Reflect => ts::SpreadMode::Reflect,
        Spread::Repeat => ts::SpreadMode::Repeat,
    };
    let [a, b, c, d, e, f] = gradient.transform;
    (stops, spread, ts::Transform::from_row(a, b, c, d, e, f))
}

#[cfg(test)]
mod tests {
    use crate::tests::{arrow, pack};
    use crate::*;

    fn alpha(image: &render::Image, x: u32, y: u32) -> u32 {
        image.pixels[(y * image.size + x) as usize] >> 24
    }

    #[test]
    fn scales_the_arrow_and_its_hotspot() {
        let pack = pack();
        let default = pack.cursor_or_default("default");
        let image = pack.rasterize(default, 0, 48).unwrap();
        assert_eq!((image.size, image.pixels.len()), (48, 48 * 48));
        assert_eq!(image.hotspot, (2, 2));
        // Inside the triangle it is black, far from it nothing.
        assert_eq!(image.pixels[(14 * 48 + 6) as usize], 0xff00_0000);
        assert_eq!(alpha(&image, 40, 40), 0);
        assert!(pack.rasterize(default, 1, 48).is_none());
    }

    #[test]
    fn strokes_the_outline_around_the_fill() {
        let pack = pack();
        let image = pack
            .rasterize(pack.cursor_or_default("default"), 0, 24)
            .unwrap();
        // The left edge runs along x = 1; its white stroke covers x = 0.5
        // to 1.75, which the fill does not reach.
        let edge = image.pixels[(8 * 24) as usize];
        assert!(edge >> 24 > 0x40, "{edge:08x}");
        assert!(edge & 0xff > 0x40, "{edge:08x}");
    }

    #[test]
    fn paints_gradients_along_their_axis() {
        let mut path = arrow();
        path.verbs = vec![Verb::Move, Verb::Line, Verb::Line, Verb::Line, Verb::Close];
        path.points = vec![(0.0, 0.0), (24.0, 0.0), (24.0, 24.0), (0.0, 24.0)];
        path.stroke = None;
        path.fill = Some(Fill {
            rule: FillRule::NonZero,
            paint: Paint::Linear {
                start: (0.0, 0.0),
                end: (24.0, 0.0),
                gradient: crate::tests::gradient(),
            },
        });
        let mut pack = pack();
        pack.cursors[0].frames[0].paths = vec![path];
        let image = pack.rasterize(&pack.cursors[0], 0, 24).unwrap();
        let (left, right) = (image.pixels[12 * 24], image.pixels[12 * 24 + 23]);
        // Red and opaque at the start, mostly blue and half transparent at
        // the end.
        assert!(
            (left >> 16) & 0xff > 0xe0 && left >> 24 > 0xf0,
            "{left:08x}"
        );
        assert!(right & 0xff > (right >> 16) & 0xff, "{right:08x}");
        assert!(right >> 24 < 0xa0, "{right:08x}");
    }
}
