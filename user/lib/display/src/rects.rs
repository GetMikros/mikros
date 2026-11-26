//! Rectangles: which parts of a plane a commit copies.

use alloc::collections::BTreeMap;
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

/// What a commit of an enabled primary plane copies to the host and
/// flushes (MIKSPEC-0031.R18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presented {
    /// The parts of the buffer to copy, in buffer coordinates.
    pub copies: Vec<Rect>,
    /// A rectangle that holds everything copied: what to flush.
    pub flush: Rect,
}

/// What a commit copies and flushes for the buffer `whole` that shows
/// `src`, with the damage `damage`: all of the buffer when `changed`, that
/// is, when the buffer, the rectangle or the mode is not what the scanout
/// was last set to, or when the commit has no damage; otherwise the damage
/// within `src`. The flush is the box around the copies, or `src` when
/// there are none.
pub fn presented(whole: &Rect, src: &Rect, damage: &[Rect], changed: bool) -> Presented {
    let copies = match changed || damage.is_empty() {
        true => alloc::vec![*whole],
        false => copied(src, damage),
    };
    let flush = copies
        .iter()
        .copied()
        .reduce(|a, b| {
            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
            let (right, bottom) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
            Rect::new(x, y, (right - x) as u64, (bottom - y) as u64)
        })
        .unwrap_or(*src);
    Presented { copies, flush }
}

/// How many rectangles of what a buffer lacks are kept before they are
/// kept as the whole buffer.
const LACKS_MOST: usize = 64;

/// What a buffer's copy on the host lacks of what its plane showed last.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Lack {
    /// All of it, or nothing is known.
    Whole,
    /// These parts, in buffer coordinates.
    Parts(Vec<Rect>),
}

impl Lack {
    /// What it lacks once `parts` of the plane changed: those too.
    fn add(&mut self, parts: &Lack) {
        match (&mut *self, parts) {
            (Lack::Whole, _) => {}
            (me, Lack::Whole) => *me = Lack::Whole,
            (Lack::Parts(mine), Lack::Parts(more)) => {
                mine.extend_from_slice(more);
                if mine.len() > LACKS_MOST {
                    *self = Lack::Whole;
                }
            }
        }
    }
}

/// What the host's copies of the buffers that a plane showed lack
/// (MIKSPEC-0031.R19): for each buffer presented on the plane, the parts
/// of it that changed on the plane since the buffer was last copied. A
/// buffer that was not presented, or whose copy failed, or that was
/// released, lacks all of it.
#[derive(Debug, Default)]
pub struct Lacking {
    by_buffer: BTreeMap<u32, Lack>,
}

impl Lacking {
    /// Records the presentation of `buffer`, of the size `whole`, showing
    /// `src`, with the damage `damage` (all of the plane when empty), and
    /// returns what it copies and flushes: all of the buffer when the
    /// plane had no earlier content (`earlier` is false) or the buffer
    /// lacks all of it, and otherwise the damage with what the buffer
    /// lacked of the commits that presented others since. Every other
    /// buffer lacks this commit's damage too from now on.
    pub fn present(
        &mut self,
        buffer: u32,
        whole: &Rect,
        src: &Rect,
        damage: &[Rect],
        earlier: bool,
    ) -> Presented {
        let parts = match damage.is_empty() {
            true => Lack::Whole,
            false => Lack::Parts(damage.iter().filter_map(|d| whole.intersect(d)).collect()),
        };
        let copies = match (earlier, self.by_buffer.get(&buffer)) {
            (true, Some(lack)) => {
                let mut all = lack.clone();
                all.add(&parts);
                match all {
                    Lack::Whole => alloc::vec![*whole],
                    Lack::Parts(list) => {
                        let list: Vec<Rect> =
                            list.iter().filter_map(|r| src.intersect(r)).collect();
                        match list.is_empty() {
                            true => alloc::vec![*src],
                            false => list,
                        }
                    }
                }
            }
            _ => alloc::vec![*whole],
        };
        if earlier {
            for (other, lack) in self.by_buffer.iter_mut() {
                if *other != buffer {
                    lack.add(&parts);
                }
            }
        } else {
            // What the others hold is unknown against what is shown now.
            self.by_buffer.clear();
        }
        self.by_buffer.insert(buffer, Lack::Parts(Vec::new()));
        let flush = copies
            .iter()
            .copied()
            .reduce(|a, b| {
                let (x, y) = (a.x.min(b.x), a.y.min(b.y));
                let (right, bottom) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
                Rect::new(x, y, (right - x) as u64, (bottom - y) as u64)
            })
            .unwrap_or(*src);
        Presented { copies, flush }
    }

    /// The copy of `buffer` failed, or the buffer was released: it counts
    /// as not presented.
    pub fn forget(&mut self, buffer: u32) {
        self.by_buffer.remove(&buffer);
    }

    /// The plane has no earlier content any more.
    pub fn reset(&mut self) {
        self.by_buffer.clear();
    }
}
