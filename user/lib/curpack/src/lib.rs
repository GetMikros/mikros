//! Cursor themes as `.curpack` files (MIKSPEC-0066.R26): the cursors of a
//! theme, each with its name, its hotspot and its frames of vector paths,
//! which xtask writes from SVG sources and the compositor reads.
//!
//! The file is little-endian and read front to back:
//!
//! ```text
//! file    = "CURPACK\0" version:u16 size:u16 count:u16 cursor*count
//! cursor  = name_len:u8 name hotspot:f32*2 frames:u16 frame*frames
//! frame   = duration_ms:u32 paths:u16 path*paths
//! path    = what:u8 [rule:u8 paint] [width:f32 join:u8 cap:u8 miter:f32 paint]
//!           verbs:u32 verb:u8*verbs points:u32 (x:f32 y:f32)*points
//! paint   = 0 rgba:u8*4
//!         | 1 x1 y1 x2 y2 spread:u8 transform:f32*6 stops
//!         | 2 cx cy r fx fy spread:u8 transform:f32*6 stops
//! stops   = count:u8 (offset:f32 rgba:u8*4)*count
//! ```
//!
//! `what` has bit 0 for a fill, which the rule and paint follow, and bit 1
//! for a stroke. Verbs are 0 move, 1 line, 2 quadratic, 3 cubic and 4
//! close; they take 1, 1, 2, 3 and 0 points. Coordinates are in the
//! theme's units, `size` of them across a cursor, with every transform of
//! the sources applied but the gradients' own. Colours are not
//! premultiplied.

use std::collections::BTreeSet;

pub const MAGIC: &[u8; 8] = b"CURPACK\0";
/// The format version that this library reads and writes.
pub const VERSION: u16 = 1;

/// Limits that keep a file from asking for absurd amounts of memory.
const MAX_CURSORS: usize = 256;
const MAX_FRAMES: usize = 256;
const MAX_NAME: usize = 64;
const MAX_STOPS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    /// The units across a cursor, which the compositor scales to the
    /// pointer's size.
    pub size: u16,
    pub cursors: Vec<Cursor>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub name: String,
    pub hotspot: (f32, f32),
    pub frames: Vec<Frame>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub duration_ms: u32,
    pub paths: Vec<Path>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub verbs: Vec<Verb>,
    pub points: Vec<(f32, f32)>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Move,
    Line,
    Quad,
    Cubic,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub rule: FillRule,
    pub paint: Paint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub width: f32,
    pub join: Join,
    pub cap: Cap,
    pub miter_limit: f32,
    pub paint: Paint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// Red, green, blue and alpha.
    Solid([u8; 4]),
    Linear {
        start: (f32, f32),
        end: (f32, f32),
        gradient: Gradient,
    },
    /// From a point of radius 0 at `focal` to the circle of `radius`
    /// around `centre`.
    Radial {
        centre: (f32, f32),
        radius: f32,
        focal: (f32, f32),
        gradient: Gradient,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub stops: Vec<Stop>,
    pub spread: Spread,
    /// The gradient's transform, `[a, b, c, d, e, f]` of
    /// `x' = a x + c y + e` and `y' = b x + d y + f`.
    pub transform: [f32; 6],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub colour: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spread {
    Pad,
    Reflect,
    Repeat,
}

/// Why a file is not a cursor theme that this library reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    BadMagic,
    /// A format version other than `VERSION`.
    UnknownVersion(u16),
    /// The file ends inside a record.
    Truncated,
    /// Bytes after the last cursor.
    TrailingBytes,
    /// A value outside its range, with what it is.
    Invalid(&'static str),
    /// No cursor `default`, which every theme holds.
    NoDefault,
    DuplicateName(String),
}

impl Pack {
    /// The cursor `name`, or `default` when the theme lacks it.
    pub fn cursor_or_default(&self, name: &str) -> &Cursor {
        self.cursor(name)
            .or_else(|| self.cursor("default"))
            .expect("a checked pack has a default cursor")
    }

    pub fn cursor(&self, name: &str) -> Option<&Cursor> {
        self.cursors.iter().find(|c| c.name == name)
    }

    /// The file of the theme, or why it cannot be one.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        self.check()?;
        let mut w = Vec::new();
        w.extend_from_slice(MAGIC);
        for value in [VERSION, self.size, self.cursors.len() as u16] {
            w.extend_from_slice(&value.to_le_bytes());
        }
        for cursor in &self.cursors {
            w.push(cursor.name.len() as u8);
            w.extend_from_slice(cursor.name.as_bytes());
            put_point(&mut w, cursor.hotspot);
            w.extend_from_slice(&(cursor.frames.len() as u16).to_le_bytes());
            for frame in &cursor.frames {
                w.extend_from_slice(&frame.duration_ms.to_le_bytes());
                w.extend_from_slice(&(frame.paths.len() as u16).to_le_bytes());
                for path in &frame.paths {
                    put_path(&mut w, path);
                }
            }
        }
        Ok(w)
    }

    /// Checks what the byte layout cannot: names, counts, finite values and
    /// paths whose verbs match their points.
    fn check(&self) -> Result<(), Error> {
        if self.size == 0 || self.cursors.len() > MAX_CURSORS {
            return Err(Error::Invalid("the size or the number of cursors"));
        }
        let mut names = BTreeSet::new();
        for cursor in &self.cursors {
            let name = &cursor.name;
            if name.is_empty() || name.len() > MAX_NAME {
                return Err(Error::Invalid("a cursor's name"));
            }
            if !names.insert(name.as_str()) {
                return Err(Error::DuplicateName(name.clone()));
            }
            finite(&[cursor.hotspot.0, cursor.hotspot.1])?;
            if cursor.frames.is_empty() || cursor.frames.len() > MAX_FRAMES {
                return Err(Error::Invalid("a cursor's number of frames"));
            }
            for frame in &cursor.frames {
                if frame.paths.len() > usize::from(u16::MAX) {
                    return Err(Error::Invalid("a frame's number of paths"));
                }
                frame.paths.iter().try_for_each(check_path)?;
            }
        }
        if !names.contains("default") {
            return Err(Error::NoDefault);
        }
        Ok(())
    }
}

impl Verb {
    fn points(self) -> usize {
        match self {
            Verb::Move | Verb::Line => 1,
            Verb::Quad => 2,
            Verb::Cubic => 3,
            Verb::Close => 0,
        }
    }
}

fn finite(values: &[f32]) -> Result<(), Error> {
    match values.iter().all(|v| v.is_finite()) {
        true => Ok(()),
        false => Err(Error::Invalid("a coordinate that is not finite")),
    }
}

fn check_path(path: &Path) -> Result<(), Error> {
    if path.fill.is_none() && path.stroke.is_none() {
        return Err(Error::Invalid("a path that is neither filled nor stroked"));
    }
    if path.verbs.first().is_some_and(|&v| v != Verb::Move) {
        return Err(Error::Invalid("a path that does not start with a move"));
    }
    let needed: usize = path.verbs.iter().map(|v| v.points()).sum();
    if needed != path.points.len() {
        return Err(Error::Invalid("a path whose points do not match its verbs"));
    }
    path.points.iter().try_for_each(|p| finite(&[p.0, p.1]))?;
    if let Some(fill) = &path.fill {
        check_paint(&fill.paint)?;
    }
    if let Some(stroke) = &path.stroke {
        finite(&[stroke.width, stroke.miter_limit])?;
        if stroke.width <= 0.0 || stroke.miter_limit < 1.0 {
            return Err(Error::Invalid("a stroke's width or miter limit"));
        }
        check_paint(&stroke.paint)?;
    }
    Ok(())
}

fn check_paint(paint: &Paint) -> Result<(), Error> {
    let gradient = match paint {
        Paint::Solid(_) => return Ok(()),
        Paint::Linear {
            start,
            end,
            gradient,
        } => {
            finite(&[start.0, start.1, end.0, end.1])?;
            gradient
        }
        Paint::Radial {
            centre,
            radius,
            focal,
            gradient,
        } => {
            finite(&[centre.0, centre.1, *radius, focal.0, focal.1])?;
            if *radius <= 0.0 {
                return Err(Error::Invalid("a radial gradient's radius"));
            }
            gradient
        }
    };
    finite(&gradient.transform)?;
    let stops = &gradient.stops;
    if stops.is_empty() || stops.len() > MAX_STOPS {
        return Err(Error::Invalid("a gradient's number of stops"));
    }
    let offsets: Vec<f32> = stops.iter().map(|s| s.offset).collect();
    finite(&offsets)?;
    let ordered = offsets.windows(2).all(|w| w[0] <= w[1]);
    if !ordered || offsets.iter().any(|o| !(0.0..=1.0).contains(o)) {
        return Err(Error::Invalid("a gradient's stop offsets"));
    }
    Ok(())
}

fn put_f32s(w: &mut Vec<u8>, values: &[f32]) {
    for v in values {
        w.extend_from_slice(&v.to_le_bytes());
    }
}

fn put_point(w: &mut Vec<u8>, p: (f32, f32)) {
    put_f32s(w, &[p.0, p.1]);
}

fn put_path(w: &mut Vec<u8>, path: &Path) {
    w.push(u8::from(path.fill.is_some()) | u8::from(path.stroke.is_some()) << 1);
    if let Some(fill) = &path.fill {
        w.push(fill.rule as u8);
        put_paint(w, &fill.paint);
    }
    if let Some(stroke) = &path.stroke {
        put_f32s(w, &[stroke.width]);
        w.extend_from_slice(&[stroke.join as u8, stroke.cap as u8]);
        put_f32s(w, &[stroke.miter_limit]);
        put_paint(w, &stroke.paint);
    }
    w.extend_from_slice(&(path.verbs.len() as u32).to_le_bytes());
    w.extend(path.verbs.iter().map(|&v| v as u8));
    w.extend_from_slice(&(path.points.len() as u32).to_le_bytes());
    for &p in &path.points {
        put_point(w, p);
    }
}

fn put_paint(w: &mut Vec<u8>, paint: &Paint) {
    let gradient = match paint {
        Paint::Solid(colour) => {
            w.push(0);
            w.extend_from_slice(colour);
            return;
        }
        Paint::Linear {
            start,
            end,
            gradient,
        } => {
            w.push(1);
            put_f32s(w, &[start.0, start.1, end.0, end.1]);
            gradient
        }
        Paint::Radial {
            centre,
            radius,
            focal,
            gradient,
        } => {
            w.push(2);
            put_f32s(w, &[centre.0, centre.1, *radius, focal.0, focal.1]);
            gradient
        }
    };
    w.push(gradient.spread as u8);
    put_f32s(w, &gradient.transform);
    w.push(gradient.stops.len() as u8);
    for stop in &gradient.stops {
        put_f32s(w, &[stop.offset]);
        w.extend_from_slice(&stop.colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    pub(crate) fn gradient() -> Gradient {
        Gradient {
            stops: vec![
                Stop {
                    offset: 0.0,
                    colour: [255, 0, 0, 255],
                },
                Stop {
                    offset: 1.0,
                    colour: [0, 0, 255, 128],
                },
            ],
            spread: Spread::Reflect,
            transform: IDENTITY,
        }
    }

    /// A triangle, filled black and stroked white.
    pub(crate) fn arrow() -> Path {
        Path {
            verbs: vec![Verb::Move, Verb::Line, Verb::Line, Verb::Close],
            points: vec![(1.0, 1.0), (1.0, 17.0), (12.0, 12.0)],
            fill: Some(Fill {
                rule: FillRule::NonZero,
                paint: Paint::Solid([0, 0, 0, 255]),
            }),
            stroke: Some(Stroke {
                width: 1.5,
                join: Join::Round,
                cap: Cap::Butt,
                miter_limit: 4.0,
                paint: Paint::Solid([255, 255, 255, 255]),
            }),
        }
    }

    pub(crate) fn pack() -> Pack {
        let curve = Path {
            verbs: vec![Verb::Move, Verb::Quad, Verb::Cubic],
            points: vec![
                (0.0, 0.0),
                (4.0, 4.0),
                (8.0, 0.0),
                (9.0, 1.0),
                (10.0, 2.0),
                (11.0, 3.0),
            ],
            fill: Some(Fill {
                rule: FillRule::EvenOdd,
                paint: Paint::Linear {
                    start: (0.0, 0.0),
                    end: (24.0, 0.0),
                    gradient: gradient(),
                },
            }),
            stroke: Some(Stroke {
                width: 2.0,
                join: Join::Miter,
                cap: Cap::Square,
                miter_limit: 4.0,
                paint: Paint::Radial {
                    centre: (12.0, 12.0),
                    radius: 8.0,
                    focal: (10.0, 10.0),
                    gradient: gradient(),
                },
            }),
        };
        let frame = |duration_ms, paths| Frame { duration_ms, paths };
        Pack {
            size: 24,
            cursors: vec![
                Cursor {
                    name: "default".into(),
                    hotspot: (1.0, 1.0),
                    frames: vec![frame(0, vec![arrow()])],
                },
                Cursor {
                    name: "wait".into(),
                    hotspot: (12.0, 12.0),
                    frames: vec![
                        frame(80, vec![curve.clone()]),
                        frame(80, vec![arrow(), curve]),
                    ],
                },
            ],
        }
    }

    #[test]
    fn writes_the_magic_and_the_version_first() {
        let bytes = pack().to_bytes().unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(bytes[8..14], [1, 0, 24, 0, 2, 0]);
        assert_eq!(pack().cursor_or_default("wait").name, "wait");
        assert_eq!(pack().cursor_or_default("text").name, "default");
    }

    #[test]
    fn refuses_themes_that_break_the_rules() {
        let broken = |change: &dyn Fn(&mut Pack)| {
            let mut pack = pack();
            change(&mut pack);
            pack.to_bytes()
        };
        assert_eq!(
            broken(&|p| p.cursors[0].name = "arrow".into()),
            Err(Error::NoDefault)
        );
        assert_eq!(
            broken(&|p| p.cursors[1].name = "default".into()),
            Err(Error::DuplicateName("default".into()))
        );
        let invalid = [
            broken(&|p| p.cursors[0].frames.clear()),
            broken(&|p| p.cursors[0].hotspot.0 = f32::NAN),
            broken(&|p| {
                p.cursors[0].frames[0].paths[0]
                    .points
                    .pop()
                    .map(drop)
                    .unwrap()
            }),
            broken(&|p| {
                let path = &mut p.cursors[0].frames[0].paths[0];
                path.fill = None;
                path.stroke = None;
            }),
            broken(&|p| p.cursors[0].frames[0].paths[0].verbs[0] = Verb::Line),
            broken(&|p| {
                p.cursors[1].frames[0].paths[0]
                    .stroke
                    .as_mut()
                    .unwrap()
                    .width = 0.0
            }),
            broken(&|p| {
                let fill = p.cursors[1].frames[0].paths[0].fill.as_mut().unwrap();
                if let Paint::Linear { gradient, .. } = &mut fill.paint {
                    gradient.stops.reverse();
                }
            }),
        ];
        for (i, result) in invalid.into_iter().enumerate() {
            assert!(
                matches!(result, Err(Error::Invalid(_))),
                "change {i}: {result:?}"
            );
        }
    }
}
