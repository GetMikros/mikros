//! Cursor themes (MIKSPEC-0066.R26): a folder of SVG drawings and a
//! `cursors.toml`, which this module turns into a `.curpack` file with
//! usvg and `mikros_curpack`. `cursors.toml` gives the theme's `size`, the
//! units across a cursor, and for each cursor, by the shape name of
//! cursor-shape, its `svg`, its `hotspot` and optionally:
//!
//! - `rotate`: degrees by which the drawing and the hotspot turn around
//!   the centre, so that one drawing serves several shapes;
//! - `frames` and `period_ms`: an animation of that many frames in that
//!   time, each turning the group whose id `spin` names, by default
//!   `spin`, a further step of a full turn around `spin_centre`, by
//!   default the centre.
//!
//! The drawings may use paths with fills and strokes of colours and of
//! linear and radial gradients, and groups with transforms; anything else,
//! such as text, images, clips, masks, filters, dashes or group opacity,
//! is refused.

use anyhow::{Context, Result, bail};
use mikros_curpack as cp;
use std::path::{Path, PathBuf};
use usvg::tiny_skia_path::{PathSegment, Point, Transform};

#[derive(clap::Args)]
pub struct Args {
    /// The folder of the theme's drawings and cursors.toml.
    source: PathBuf,
    /// Where to write a PNG of every cursor and frame at 24, 32 and 48
    /// pixels, a row for each cursor.
    #[arg(long)]
    preview: Option<PathBuf>,
}

pub fn run(args: &Args) -> Result<()> {
    let pack = build(&args.source)?;
    println!("built    {}", pack.display());
    if let Some(out) = &args.preview {
        let pack = cp::Pack::parse(&std::fs::read(&pack)?)
            .map_err(|e| anyhow::anyhow!("reading back {}: {e:?}", pack.display()))?;
        std::fs::write(out, preview(&pack))?;
        println!("drew     {}", out.display());
    }
    Ok(())
}

/// The sizes in pixels of the preview, and the side of its cells.
const SIZES: [u32; 3] = [24, 32, 48];
const CELL: u32 = 56;

/// A PNG of the theme's cursors on a grey checkerboard, each frame at
/// each of `SIZES`, with the hotspot marked by a red pixel.
fn preview(pack: &cp::Pack) -> Vec<u8> {
    let columns = pack
        .cursors
        .iter()
        .map(|c| c.frames.len())
        .max()
        .unwrap_or(1) as u32;
    let (width, height) = (
        columns * SIZES.len() as u32 * CELL,
        pack.cursors.len() as u32 * CELL,
    );
    let mut rgb = vec![0u8; (width * height * 3) as usize];
    for y in 0..height {
        for x in 0..width {
            let grey = if (x / 8 + y / 8) % 2 == 0 { 200 } else { 230 };
            let at = ((y * width + x) * 3) as usize;
            rgb[at..at + 3].fill(grey);
        }
    }
    for (row, cursor) in pack.cursors.iter().enumerate() {
        for (f, _) in cursor.frames.iter().enumerate() {
            for (s, &size) in SIZES.iter().enumerate() {
                let image = pack
                    .rasterize(cursor, f, size)
                    .expect("a frame of a cursor");
                let left = (f as u32 * SIZES.len() as u32 + s as u32) * CELL + 4;
                let top = row as u32 * CELL + 4;
                for (i, &p) in image.pixels.iter().enumerate() {
                    let (x, y) = (left + i as u32 % size, top + i as u32 / size);
                    let at = ((y * width + x) * 3) as usize;
                    let alpha = p >> 24;
                    for (k, shift) in [16, 8, 0].into_iter().enumerate() {
                        let c = (p >> shift) & 0xff;
                        let under = u32::from(rgb[at + k]);
                        rgb[at + k] = (c + under * (255 - alpha) / 255) as u8;
                    }
                }
                let (hx, hy) = image.hotspot;
                if (0..size as i32).contains(&hx) && (0..size as i32).contains(&hy) {
                    let at = (((top + hy as u32) * width + left + hx as u32) * 3) as usize;
                    rgb[at..at + 3].copy_from_slice(&[255, 0, 0]);
                }
            }
        }
    }
    png(width, height, &rgb)
}

/// A PNG of `width` by `height` RGB pixels.
fn png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut raw = Vec::with_capacity(rgb.len() + height as usize);
    for row in rgb.chunks_exact(width as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).expect("writing to memory");
    let data = z.finish().expect("writing to memory");
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    for (kind, body) in [(b"IHDR", header), (b"IDAT", data), (b"IEND", Vec::new())] {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut chunk = kind.to_vec();
        chunk.extend_from_slice(&body);
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc32fast::hash(&chunk).to_be_bytes());
    }
    out
}

/// Writes the theme of the folder `source` to
/// `target/cursors/<folder>.curpack`, and returns that path.
pub fn build(source: &Path) -> Result<PathBuf> {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .context("a cursor theme's folder without a name")?;
    let manifest = std::fs::read_to_string(source.join("cursors.toml"))
        .with_context(|| format!("reading {}/cursors.toml", source.display()))?;
    let read =
        |file: &str| std::fs::read(source.join(file)).with_context(|| format!("reading {file}"));
    let pack = convert(&manifest, &read)
        .with_context(|| format!("the cursor theme {}", source.display()))?;
    let bytes = pack
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("the cursor theme {}: {e:?}", source.display()))?;
    let dir = crate::workspace_root().join("target/cursors");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!("{name}.curpack"));
    std::fs::write(&out, bytes)?;
    Ok(out)
}

/// The theme of the manifest `manifest`, whose drawings `read` reads.
fn convert(manifest: &str, read: &dyn Fn(&str) -> Result<Vec<u8>>) -> Result<cp::Pack> {
    let table: toml::Table = manifest.parse()?;
    let size = table
        .get("size")
        .and_then(|s| s.as_integer())
        .and_then(|s| u16::try_from(s).ok())
        .context("`size` is not a number of units")?;
    let centre = f32::from(size) / 2.0;
    let cursors = table
        .get("cursors")
        .and_then(|c| c.as_table())
        .context("no [cursors]")?;
    let mut pack = cp::Pack {
        size,
        cursors: Vec::new(),
    };
    for (name, cursor) in cursors {
        let cursor = cursor
            .as_table()
            .with_context(|| format!("{name}: not a table"))?;
        let number = |key: &str| -> Result<Option<f64>> {
            match cursor.get(key) {
                None => Ok(None),
                Some(v) => v
                    .as_float()
                    .or_else(|| v.as_integer().map(|i| i as f64))
                    .map(Some)
                    .with_context(|| format!("{name}: `{key}` is not a number")),
            }
        };
        let pair = |key: &str| -> Result<Option<(f32, f32)>> {
            let Some(v) = cursor.get(key) else {
                return Ok(None);
            };
            let xy: Option<Vec<f32>> = v.as_array().map(|a| {
                a.iter()
                    .filter_map(|n| n.as_float().or_else(|| n.as_integer().map(|i| i as f64)))
                    .map(|n| n as f32)
                    .collect()
            });
            match xy.as_deref() {
                Some(&[x, y]) => Ok(Some((x, y))),
                _ => bail!("{name}: `{key}` is not [x, y]"),
            }
        };
        for key in cursor.keys() {
            let known = [
                "svg",
                "hotspot",
                "rotate",
                "frames",
                "period_ms",
                "spin",
                "spin_centre",
            ];
            if !known.contains(&key.as_str()) {
                bail!("{name}: unknown key `{key}`");
            }
        }
        let svg = cursor
            .get("svg")
            .and_then(|s| s.as_str())
            .with_context(|| format!("{name}: no `svg`"))?;
        let hotspot = pair("hotspot")?.with_context(|| format!("{name}: no `hotspot`"))?;
        let turn =
            Transform::from_rotate_at(number("rotate")?.unwrap_or(0.0) as f32, centre, centre);
        let frames = number("frames")?.unwrap_or(1.0) as u32;
        let period = number("period_ms")?.unwrap_or(0.0) as u32;
        if frames == 0 || (frames > 1 && period == 0) {
            bail!("{name}: an animation needs frames and a period");
        }
        let spin = cursor
            .get("spin")
            .and_then(|s| s.as_str())
            .unwrap_or("spin");
        let (sx, sy) = pair("spin_centre")?.unwrap_or((centre, centre));
        let options = usvg::Options::default();
        let tree = usvg::Tree::from_data(&read(svg)?, &options)
            .with_context(|| format!("{name}: parsing {svg}"))?;
        let mut hot = Point::from_xy(hotspot.0, hotspot.1);
        turn.map_point(&mut hot);
        let mut out = cp::Cursor {
            name: name.clone(),
            hotspot: (hot.x, hot.y),
            frames: Vec::new(),
        };
        for frame in 0..frames {
            let step = Transform::from_rotate_at(360.0 * frame as f32 / frames as f32, sx, sy);
            let mut paths = Vec::new();
            let spinning = Spin { id: spin, step };
            group(tree.root(), turn, &spinning, false, &mut paths)
                .with_context(|| format!("{name}: {svg}"))?;
            out.frames.push(cp::Frame {
                duration_ms: if frames > 1 { period / frames } else { 0 },
                paths,
            });
        }
        pack.cursors.push(out);
    }
    Ok(pack)
}

/// The group that an animation turns, and its turn in this frame.
struct Spin<'a> {
    id: &'a str,
    step: Transform,
}

/// Adds the paths below `g` to `paths`, turned by `turn` and, inside the
/// spinning group (`spinning`), by the frame's step.
fn group(
    g: &usvg::Group,
    turn: Transform,
    spin: &Spin,
    spinning: bool,
    paths: &mut Vec<cp::Path>,
) -> Result<()> {
    if g.opacity().get() != 1.0 || g.clip_path().is_some() || g.mask().is_some() {
        bail!("a group with opacity, a clip or a mask");
    }
    if !g.filters().is_empty() {
        bail!("a group with filters");
    }
    let spinning = spinning || (!g.id().is_empty() && g.id() == spin.id);
    for node in g.children() {
        match node {
            usvg::Node::Group(inner) => group(inner, turn, spin, spinning, paths)?,
            usvg::Node::Path(p) if p.is_visible() => {
                let mut total = turn;
                if spinning {
                    total = total.pre_concat(spin.step);
                }
                path(p, total.pre_concat(p.abs_transform()), paths)?;
            }
            usvg::Node::Path(_) => {}
            usvg::Node::Image(_) | usvg::Node::Text(_) => bail!("an image or a text"),
        }
    }
    Ok(())
}

/// Adds `p`, transformed by `transform`, as one path, or as a stroke and
/// then a fill when the drawing strokes first.
fn path(p: &usvg::Path, transform: Transform, paths: &mut Vec<cp::Path>) -> Result<()> {
    let data = p
        .data()
        .clone()
        .transform(transform)
        .context("a path that cannot be transformed")?;
    let mut verbs = Vec::new();
    let mut points = Vec::new();
    for segment in data.segments() {
        let (verb, at): (cp::Verb, &[Point]) = match &segment {
            PathSegment::MoveTo(a) => (cp::Verb::Move, std::slice::from_ref(a)),
            PathSegment::LineTo(a) => (cp::Verb::Line, std::slice::from_ref(a)),
            PathSegment::QuadTo(a, b) => (cp::Verb::Quad, &[*a, *b][..]),
            PathSegment::CubicTo(a, b, c) => (cp::Verb::Cubic, &[*a, *b, *c][..]),
            PathSegment::Close => (cp::Verb::Close, &[][..]),
        };
        verbs.push(verb);
        points.extend(at.iter().map(|p| (p.x, p.y)));
    }
    let fill = match p.fill() {
        None => None,
        Some(f) => Some(cp::Fill {
            rule: match f.rule() {
                usvg::FillRule::NonZero => cp::FillRule::NonZero,
                usvg::FillRule::EvenOdd => cp::FillRule::EvenOdd,
            },
            paint: paint(f.paint(), f.opacity().get(), transform)?,
        }),
    };
    let stroke = match p.stroke() {
        None => None,
        Some(s) => {
            if s.dasharray().is_some() {
                bail!("a dashed stroke");
            }
            Some(cp::Stroke {
                width: s.width().get(),
                join: match s.linejoin() {
                    usvg::LineJoin::Round => cp::Join::Round,
                    usvg::LineJoin::Bevel => cp::Join::Bevel,
                    _ => cp::Join::Miter,
                },
                cap: match s.linecap() {
                    usvg::LineCap::Butt => cp::Cap::Butt,
                    usvg::LineCap::Round => cp::Cap::Round,
                    usvg::LineCap::Square => cp::Cap::Square,
                },
                miter_limit: s.miterlimit().get(),
                paint: paint(s.paint(), s.opacity().get(), transform)?,
            })
        }
    };
    let shape = |fill, stroke| cp::Path {
        verbs: verbs.clone(),
        points: points.clone(),
        fill,
        stroke,
    };
    match p.paint_order() {
        usvg::PaintOrder::StrokeAndFill if fill.is_some() && stroke.is_some() => {
            paths.push(shape(None, stroke));
            paths.push(shape(fill, None));
        }
        _ => paths.push(shape(fill, stroke)),
    }
    Ok(())
}

/// The paint of `paint` at `opacity`, whose gradients take the path's
/// `transform` before their own.
fn paint(paint: &usvg::Paint, opacity: f32, transform: Transform) -> Result<cp::Paint> {
    let colour =
        |c: usvg::Color, alpha: f32| [c.red, c.green, c.blue, (alpha * 255.0).round() as u8];
    let gradient = |base: &usvg::BaseGradient| -> cp::Gradient {
        let t = transform.pre_concat(base.transform());
        cp::Gradient {
            stops: base
                .stops()
                .iter()
                .map(|s| cp::Stop {
                    offset: s.offset().get(),
                    colour: colour(s.color(), s.opacity().get() * opacity),
                })
                .collect(),
            spread: match base.spread_method() {
                usvg::SpreadMethod::Pad => cp::Spread::Pad,
                usvg::SpreadMethod::Reflect => cp::Spread::Reflect,
                usvg::SpreadMethod::Repeat => cp::Spread::Repeat,
            },
            transform: [t.sx, t.ky, t.kx, t.sy, t.tx, t.ty],
        }
    };
    Ok(match paint {
        usvg::Paint::Color(c) => cp::Paint::Solid(colour(*c, opacity)),
        usvg::Paint::LinearGradient(l) => cp::Paint::Linear {
            start: (l.x1(), l.y1()),
            end: (l.x2(), l.y2()),
            gradient: gradient(l),
        },
        usvg::Paint::RadialGradient(r) => {
            if r.fr().get() != 0.0 {
                bail!("a radial gradient with a focal radius");
            }
            cp::Paint::Radial {
                centre: (r.cx(), r.cy()),
                radius: r.r().get(),
                focal: (r.fx(), r.fy()),
                gradient: gradient(r),
            }
        }
        usvg::Paint::Pattern(_) => bail!("a pattern"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARROW: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
        <path d="M4 3 L4 19 L9 15 Z" fill="#000" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"/>
    </svg>"##;
    const SPINNER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
        <defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f" stop-opacity="0.5"/></linearGradient></defs>
        <g id="spin"><rect x="11" y="2" width="2" height="6" fill="url(#g)"/></g>
    </svg>"##;

    fn read(file: &str) -> Result<Vec<u8>> {
        match file {
            "arrow.svg" => Ok(ARROW.as_bytes().to_vec()),
            "spinner.svg" => Ok(SPINNER.as_bytes().to_vec()),
            _ => bail!("no {file}"),
        }
    }

    #[test]
    fn converts_drawings_rotations_and_animations() {
        let manifest = r#"
            size = 24
            [cursors.default]
            svg = "arrow.svg"
            hotspot = [4, 3]
            [cursors.e_resize]
            svg = "arrow.svg"
            hotspot = [4, 3]
            rotate = 180
            [cursors.wait]
            svg = "spinner.svg"
            hotspot = [12, 12]
            frames = 4
            period_ms = 800
        "#;
        let pack = convert(manifest, &read).unwrap();
        pack.to_bytes().unwrap();
        let default = pack.cursor("default").unwrap();
        assert_eq!(default.hotspot, (4.0, 3.0));
        let path = &default.frames[0].paths[0];
        assert_eq!(path.verbs.first(), Some(&cp::Verb::Move));
        assert_eq!(path.points[0], (4.0, 3.0));
        assert_eq!(
            path.fill.as_ref().unwrap().paint,
            cp::Paint::Solid([0, 0, 0, 255])
        );
        let stroke = path.stroke.as_ref().unwrap();
        assert_eq!((stroke.width, stroke.join), (1.5, cp::Join::Round));
        // Turned half around the centre.
        let turned = pack.cursor("e_resize").unwrap();
        let (x, y) = turned.hotspot;
        assert!(
            (x - 20.0).abs() < 1e-4 && (y - 21.0).abs() < 1e-4,
            "{x} {y}"
        );
        let wait = pack.cursor("wait").unwrap();
        assert_eq!(wait.frames.len(), 4);
        assert_eq!(wait.frames[1].duration_ms, 200);
        let first = &wait.frames[0].paths[0].points;
        let second = &wait.frames[1].paths[0].points;
        assert_ne!(first, second);
        let cp::Paint::Linear { gradient, .. } =
            &wait.frames[0].paths[0].fill.as_ref().unwrap().paint
        else {
            panic!("a linear gradient");
        };
        assert_eq!(gradient.stops[1].colour, [0, 0, 255, 128]);
    }

    #[test]
    fn refuses_what_the_format_cannot_hold() {
        let text = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0 L4 4" stroke="#000" stroke-dasharray="1 1"/></svg>"##;
        let read = |_: &str| Ok(text.as_bytes().to_vec());
        let manifest = "size = 24\n[cursors.default]\nsvg = \"x.svg\"\nhotspot = [0, 0]\n";
        assert!(convert(manifest, &read).is_err());
        let unknown =
            "size = 24\n[cursors.default]\nsvg = \"arrow.svg\"\nhotspot = [0, 0]\ncolour = 1\n";
        assert!(convert(unknown, &super::tests::read).is_err());
    }
}
