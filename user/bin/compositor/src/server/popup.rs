//! `xdg_positioner` (MIKSPEC-0066.R16): the rules by which a popup is
//! placed against a rectangle of its parent: the popup's size, the anchor
//! rectangle and the point of it that the popup is anchored to, the
//! direction in which the popup extends from that point, an offset, and
//! what to do when the popup would lie beyond the output: flip to the
//! other side, slide along the edge, or shrink, in that order.

use super::{ClientId, ProtocolError, Rect, Request, Server};
use crate::wayland::protocols::xdg_positioner;
use crate::wayland::wire::{Arg, ObjectId};
use std::collections::BTreeMap;

const SLIDE_X: u32 = 1;
const SLIDE_Y: u32 = 2;
const FLIP_X: u32 = 4;
const FLIP_Y: u32 = 8;
const RESIZE_X: u32 = 16;
const RESIZE_Y: u32 = 32;
/// The highest anchor and gravity.
const EDGES_MAX: u32 = 8;
const CONSTRAINTS_ALL: u32 = SLIDE_X | SLIDE_Y | FLIP_X | FLIP_Y | RESIZE_X | RESIZE_Y;

/// What a client set on a positioner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Positioner {
    pub size: Option<(i32, i32)>,
    pub anchor_rect: Option<Rect>,
    pub anchor: u32,
    pub gravity: u32,
    pub adjustment: u32,
    pub offset: (i32, i32),
    pub reactive: bool,
    pub parent_size: Option<(i32, i32)>,
}

/// The horizontal and vertical direction of an anchor or gravity value,
/// each -1, 0 or 1: left and top are -1.
fn direction(edge: u32) -> (i32, i32) {
    match edge {
        1 => (0, -1),
        2 => (0, 1),
        3 => (-1, 0),
        4 => (1, 0),
        5 => (-1, -1),
        6 => (-1, 1),
        7 => (1, -1),
        8 => (1, 1),
        _ => (0, 0),
    }
}

impl Positioner {
    /// Whether the size and the anchor rectangle were set, which a popup
    /// needs.
    pub fn is_complete(&self) -> bool {
        self.size.is_some() && self.anchor_rect.is_some()
    }

    /// The popup's rectangle, relative to its parent's window geometry, for
    /// the anchor and gravity directions `anchor` and `gravity`.
    fn unconstrained(&self, anchor: (i32, i32), gravity: (i32, i32)) -> Rect {
        let (width, height) = self.size.unwrap_or((0, 0));
        let rect = self.anchor_rect.unwrap_or(Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
        let point = |start: i32, extent: i32, direction: i32| start + extent * (direction + 1) / 2;
        let (px, py) = (
            point(rect.x, rect.width, anchor.0),
            point(rect.y, rect.height, anchor.1),
        );
        Rect {
            x: px - width * (1 - gravity.0) / 2 + self.offset.0,
            y: py - height * (1 - gravity.1) / 2 + self.offset.1,
            width,
            height,
        }
    }

    /// The popup's rectangle relative to its parent's window geometry,
    /// moved into `bounds`, which is in the same coordinates, as its
    /// adjustments allow: flipped across the anchor, slid, and shrunk.
    pub fn place(&self, bounds: Rect) -> Rect {
        let (anchor, gravity) = (direction(self.anchor), direction(self.gravity));
        let mut rect = self.unconstrained(anchor, gravity);
        let beyond_x = |r: &Rect| r.x < bounds.x || r.x + r.width > bounds.x + bounds.width;
        let beyond_y = |r: &Rect| r.y < bounds.y || r.y + r.height > bounds.y + bounds.height;
        let (mut anchor_now, mut gravity_now) = (anchor, gravity);
        if self.adjustment & FLIP_X != 0 && beyond_x(&rect) {
            let flipped = self.unconstrained(
                (-anchor_now.0, anchor_now.1),
                (-gravity_now.0, gravity_now.1),
            );
            if !beyond_x(&flipped) {
                anchor_now.0 = -anchor_now.0;
                gravity_now.0 = -gravity_now.0;
                rect.x = flipped.x;
            }
        }
        if self.adjustment & FLIP_Y != 0 && beyond_y(&rect) {
            let flipped = self.unconstrained(
                (anchor_now.0, -anchor_now.1),
                (gravity_now.0, -gravity_now.1),
            );
            if !beyond_y(&flipped) {
                rect.y = flipped.y;
            }
        }
        if self.adjustment & SLIDE_X != 0 && beyond_x(&rect) {
            let right = bounds.x + bounds.width;
            if rect.x + rect.width > right {
                rect.x = right - rect.width;
            }
            if rect.x < bounds.x {
                rect.x = bounds.x;
            }
        }
        if self.adjustment & SLIDE_Y != 0 && beyond_y(&rect) {
            let bottom = bounds.y + bounds.height;
            if rect.y + rect.height > bottom {
                rect.y = bottom - rect.height;
            }
            if rect.y < bounds.y {
                rect.y = bounds.y;
            }
        }
        if self.adjustment & RESIZE_X != 0 && beyond_x(&rect) {
            let left = rect.x.max(bounds.x);
            let right = (rect.x + rect.width).min(bounds.x + bounds.width);
            rect.x = left;
            rect.width = (right - left).max(1);
        }
        if self.adjustment & RESIZE_Y != 0 && beyond_y(&rect) {
            let top = rect.y.max(bounds.y);
            let bottom = (rect.y + rect.height).min(bounds.y + bounds.height);
            rect.y = top;
            rect.height = (bottom - top).max(1);
        }
        rect
    }
}

/// The positioners of every client.
#[derive(Debug, Default)]
pub(super) struct Popups {
    positioners: BTreeMap<(ClientId, ObjectId), Positioner>,
}

impl Server {
    /// Makes the positioner `object` of `client`, empty.
    pub(super) fn new_positioner(&mut self, client: ClientId, object: ObjectId) {
        self.popups
            .positioners
            .insert((client, object), Positioner::default());
    }

    /// The positioner `object` of `client`.
    pub(super) fn positioner(&self, client: ClientId, object: ObjectId) -> Option<Positioner> {
        self.popups.positioners.get(&(client, object)).copied()
    }

    pub(super) fn positioner_request(
        &mut self,
        request: Request,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded = xdg_positioner::Request::decode(request.opcode, args)
            .map_err(|e| ProtocolError::invalid_method(request.object, format!("{e:?}")))?;
        let invalid = |message: &str| {
            ProtocolError::new(
                request.object,
                xdg_positioner::error::INVALID_INPUT,
                message,
            )
        };
        let key = (request.client, request.object);
        if let xdg_positioner::Request::Destroy = decoded {
            self.popups.positioners.remove(&key);
            self.client_mut(request).destroy(request.object);
            return Ok(());
        }
        let Some(p) = self.popups.positioners.get_mut(&key) else {
            return Ok(());
        };
        match decoded {
            xdg_positioner::Request::Destroy => unreachable!("handled above"),
            xdg_positioner::Request::SetSize { width, height } => {
                if width <= 0 || height <= 0 {
                    return Err(invalid("a size without area"));
                }
                p.size = Some((width, height));
            }
            xdg_positioner::Request::SetAnchorRect {
                x,
                y,
                width,
                height,
            } => {
                if width <= 0 || height <= 0 {
                    return Err(invalid("an anchor rectangle without area"));
                }
                p.anchor_rect = Some(Rect {
                    x,
                    y,
                    width,
                    height,
                });
            }
            xdg_positioner::Request::SetAnchor { anchor } => {
                if anchor > EDGES_MAX {
                    return Err(invalid("an anchor that does not exist"));
                }
                p.anchor = anchor;
            }
            xdg_positioner::Request::SetGravity { gravity } => {
                if gravity > EDGES_MAX {
                    return Err(invalid("a gravity that does not exist"));
                }
                p.gravity = gravity;
            }
            xdg_positioner::Request::SetConstraintAdjustment {
                constraint_adjustment,
            } => p.adjustment = constraint_adjustment & CONSTRAINTS_ALL,
            xdg_positioner::Request::SetOffset { x, y } => p.offset = (x, y),
            xdg_positioner::Request::SetReactive => p.reactive = true,
            xdg_positioner::Request::SetParentSize {
                parent_width,
                parent_height,
            } => p.parent_size = Some((parent_width, parent_height)),
            xdg_positioner::Request::SetParentConfigure { .. } => {}
        }
        Ok(())
    }

    /// Forgets the positioners of `client`, which went.
    pub(super) fn forget_client_popups(&mut self, client: ClientId) {
        self.popups.positioners.retain(|(c, _), _| *c != client);
    }
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

    /// A 100 by 50 popup anchored to a 20 by 10 rectangle at (200, 100).
    fn positioner(anchor: u32, gravity: u32, adjustment: u32) -> Positioner {
        Positioner {
            size: Some((100, 50)),
            anchor_rect: Some(rect(200, 100, 20, 10)),
            anchor,
            gravity,
            adjustment,
            ..Positioner::default()
        }
    }

    const BOUNDS: Rect = Rect {
        x: 0,
        y: 0,
        width: 1000,
        height: 800,
    };

    #[test]
    fn places_the_popup_by_the_anchor_and_the_gravity() {
        // Below the rectangle's bottom left corner, extending right and down.
        assert_eq!(positioner(6, 8, 0).place(BOUNDS), rect(200, 110, 100, 50));
        // Above its top right corner, extending left and up.
        assert_eq!(positioner(7, 5, 0).place(BOUNDS), rect(120, 50, 100, 50));
        // Centred on it, with no direction at all.
        assert_eq!(positioner(0, 0, 0).place(BOUNDS), rect(160, 80, 100, 50));
        // To the right of it, centred on its height.
        assert_eq!(positioner(4, 4, 0).place(BOUNDS), rect(220, 80, 100, 50));
        // An offset moves it.
        let offset = Positioner {
            offset: (3, -4),
            ..positioner(6, 8, 0)
        };
        assert_eq!(offset.place(BOUNDS), rect(203, 106, 100, 50));
    }

    #[test]
    fn flips_slides_and_shrinks_into_the_bounds() {
        // Below the rectangle it would reach past the bottom: flipped it lies above.
        let low = Positioner {
            anchor_rect: Some(rect(200, 760, 20, 10)),
            ..positioner(6, 8, FLIP_Y)
        };
        assert_eq!(low.place(BOUNDS), rect(200, 710, 100, 50));
        // A flip that does not fit either is not taken.
        let small = Rect {
            height: 70,
            ..BOUNDS
        };
        let both = Positioner {
            anchor_rect: Some(rect(200, 30, 20, 10)),
            ..positioner(6, 8, FLIP_Y)
        };
        assert_eq!(both.place(small), rect(200, 40, 100, 50));
        // Slid along the right edge.
        let right = Positioner {
            anchor_rect: Some(rect(950, 100, 20, 10)),
            ..positioner(6, 8, SLIDE_X)
        };
        assert_eq!(right.place(BOUNDS), rect(900, 110, 100, 50));
        // Shrunk where nothing else helps.
        let wide = Positioner {
            anchor_rect: Some(rect(950, 100, 20, 10)),
            ..positioner(6, 8, RESIZE_X)
        };
        assert_eq!(wide.place(BOUNDS), rect(950, 110, 50, 50));
        // Without adjustments it stays beyond.
        assert_eq!(
            Positioner {
                adjustment: 0,
                ..right
            }
            .place(BOUNDS),
            rect(950, 110, 100, 50)
        );
    }

    #[test]
    fn a_positioner_is_complete_with_a_size_and_an_anchor_rectangle() {
        let mut p = Positioner::default();
        assert!(!p.is_complete());
        p.size = Some((1, 1));
        assert!(!p.is_complete());
        p.anchor_rect = Some(rect(0, 0, 1, 1));
        assert!(p.is_complete());
    }
}
