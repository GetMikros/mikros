//! Resizing windows with the pointer (MIKSPEC-0074.R9, MIKSPEC-0074.R15,
//! MIKSPEC-0074.R16).

use super::{Rect, SurfaceId};

/// A window that the pointer resizes: the frame it had, the pointer's
/// place, the edges that move and the button that holds the resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Resize {
    pub surface: SurfaceId,
    pub edges: u32,
    pub frame: Rect,
    pub pointer: (i32, i32),
    pub button: u32,
}
