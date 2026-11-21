//! The pointer (MIKSPEC-0066.R28): a cursor of the theme's cursor theme
//! (MIKSPEC-0066.R26), rasterized at the pointer size of the
//! accessibility settings (MIKSPEC-0102.R8) and the output's scale of 1,
//! with its hotspot at the pointer's position, the top layer of every
//! frame. A theme whose file is missing, has an unknown format version or
//! is invalid is logged when the theme is chosen, and a built-in arrow is
//! drawn instead. The server says where the pointer is and which shape
//! the client under it chose.

use super::host::HostBuffer;
use super::render::Renderer;
use crate::accessibility::POINTER_SIZE;
use mikros_curpack as cp;

/// Where cursor themes are installed.
const THEMES: &str = "/system/share/cursors";

/// What a rasterized frame shows: the theme, the cursor, the frame and
/// the size in pixels.
#[derive(PartialEq, Eq)]
struct Key {
    theme: String,
    cursor: String,
    frame: usize,
    size: u32,
}

pub struct Pointer {
    theme: String,
    pack: cp::Pack,
    size: u32,
    /// The last frame rasterized, and its hotspot in pixels.
    image: Option<(Key, HostBuffer, (i32, i32))>,
}

impl Pointer {
    pub fn new() -> Pointer {
        Pointer {
            theme: String::new(),
            pack: built_in(),
            size: POINTER_SIZE,
            image: None,
        }
    }

    /// Uses the cursor theme `name`, and loads it when it changed.
    pub fn set_theme(&mut self, name: &str, renderer: &Renderer) -> bool {
        if name == self.theme {
            return false;
        }
        self.theme = name.into();
        self.pack = load(name);
        self.forget(renderer);
        true
    }

    /// Draws the pointer `size` logical pixels across.
    pub fn set_size(&mut self, size: u32, renderer: &Renderer) -> bool {
        if size == self.size {
            return false;
        }
        self.size = size;
        self.forget(renderer);
        true
    }

    fn forget(&mut self, renderer: &Renderer) {
        if let Some((_, buffer, _)) = self.image.take() {
            renderer.forget_host(buffer);
        }
    }

    /// The cursor `shape` at `now_ms` with its hotspot at `at`: its
    /// pixels, where their corner goes, and the milliseconds until its
    /// next frame when it is animated.
    pub fn image(
        &mut self,
        renderer: &Renderer,
        now_ms: u64,
        shape: &str,
        at: (i32, i32),
    ) -> Option<(&HostBuffer, (i32, i32), Option<u64>)> {
        let cursor = self.pack.cursor_or_default(shape);
        let (frame, next) = frame_at(cursor, now_ms);
        let key = Key {
            theme: self.theme.clone(),
            cursor: cursor.name.clone(),
            frame,
            size: self.size,
        };
        if self.image.as_ref().is_none_or(|(k, _, _)| *k != key) {
            self.forget(renderer);
            let cursor = self.pack.cursor_or_default(shape);
            let image = self.pack.rasterize(cursor, frame, self.size)?;
            let mut buffer = renderer
                .host_pixels(&image.pixels, image.size, image.size)
                .ok()?;
            buffer.alpha = true;
            self.image = Some((key, buffer, image.hotspot));
        }
        let (_, buffer, (hx, hy)) = self.image.as_ref()?;
        Some((buffer, (at.0 - hx, at.1 - hy), next))
    }
}

/// The frame of `cursor` at `now_ms` and the milliseconds left of it, or
/// frame 0 for ever when the cursor is not animated.
fn frame_at(cursor: &cp::Cursor, now_ms: u64) -> (usize, Option<u64>) {
    let total: u64 = cursor.frames.iter().map(|f| u64::from(f.duration_ms)).sum();
    if cursor.frames.len() < 2 || total == 0 {
        return (0, None);
    }
    let mut at = now_ms % total;
    for (i, frame) in cursor.frames.iter().enumerate() {
        let duration = u64::from(frame.duration_ms);
        if at < duration {
            return (i, Some(duration - at));
        }
        at -= duration;
    }
    (0, None)
}

/// The cursor theme `name`, or the built-in arrow when it cannot be read.
fn load(name: &str) -> cp::Pack {
    let path = format!("{THEMES}/{name}.curpack");
    let pack = std::fs::read(&path)
        .map_err(|e| format!("{e}"))
        .and_then(|bytes| cp::Pack::parse(&bytes).map_err(|e| format!("{e:?}")));
    match pack {
        Ok(pack) => pack,
        Err(e) => {
            mikros_rt::log!(
                Error,
                "compositor: the cursor theme {path}: {e}; drawing an arrow"
            );
            built_in()
        }
    }
}

/// The built-in arrow: a black arrow outlined in white.
fn built_in() -> cp::Pack {
    let arrow = cp::Path {
        verbs: vec![
            cp::Verb::Move,
            cp::Verb::Line,
            cp::Verb::Line,
            cp::Verb::Line,
            cp::Verb::Line,
            cp::Verb::Line,
            cp::Verb::Line,
            cp::Verb::Close,
        ],
        points: vec![
            (4.0, 3.0),
            (4.0, 19.0),
            (8.2, 15.2),
            (11.0, 21.0),
            (13.6, 19.8),
            (10.9, 14.1),
            (16.6, 14.0),
        ],
        fill: Some(cp::Fill {
            rule: cp::FillRule::NonZero,
            paint: cp::Paint::Solid([0, 0, 0, 255]),
        }),
        stroke: Some(cp::Stroke {
            width: 1.5,
            join: cp::Join::Round,
            cap: cp::Cap::Butt,
            miter_limit: 4.0,
            paint: cp::Paint::Solid([255, 255, 255, 255]),
        }),
    };
    cp::Pack {
        size: 24,
        cursors: vec![cp::Cursor {
            name: "default".into(),
            hotspot: (4.0, 3.0),
            frames: vec![cp::Frame {
                duration_ms: 0,
                paths: vec![arrow],
            }],
        }],
    }
}
