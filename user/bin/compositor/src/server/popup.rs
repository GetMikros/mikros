//! `xdg_positioner` (MIKSPEC-0066.R16): the rules by which a popup is
//! placed against a rectangle of its parent: the popup's size, the anchor
//! rectangle and the point of it that the popup is anchored to, the
//! direction in which the popup extends from that point, an offset, and
//! what to do when the popup would lie beyond the output: flip to the
//! other side, slide along the edge, or shrink, in that order. And
//! `xdg_popup`: a surface with the role of a popup lies at the place that
//! its positioner gives, against the window geometry of its parent, above
//! it, once its client acknowledged the configure that the initial commit
//! brings, and until its parent goes or the popup is dismissed.

use super::{ClientId, ProtocolError, Rect, Request, Resource, Role, Server, SurfaceId};
use crate::wayland::protocols::{wl_seat, xdg_popup, xdg_positioner, xdg_surface, xdg_wm_base};
use crate::wayland::wire::{Arg, NewId, ObjectId};
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

/// A popup.
#[derive(Debug)]
pub(super) struct Popup {
    client: ClientId,
    /// Its xdg_popup object.
    object: ObjectId,
    /// What it is a popup of, once that is known: an xdg_surface that has
    /// a role, or a layer surface.
    pub(super) parent: Option<SurfaceId>,
    positioner: Positioner,
    /// Where it lies, relative to the window geometry of its parent.
    pub(super) rect: Rect,
    /// Whether the compositor dismissed it with `popup_done`.
    pub(super) dismissed: bool,
    /// Whether it grabbed the seat: a press outside its client dismisses it.
    grabbed: bool,
}

/// The positioners of every client, and the popups.
#[derive(Debug, Default)]
pub(super) struct Popups {
    positioners: BTreeMap<(ClientId, ObjectId), Positioner>,
    pub(super) popups: BTreeMap<SurfaceId, Popup>,
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

    /// Makes the popup `object` of the surface `surface`, which is a popup
    /// of `parent` (an xdg_surface object) if that is given, placed by
    /// `positioner`.
    pub(super) fn get_popup(
        &mut self,
        request: Request,
        surface: SurfaceId,
        id: NewId,
        parent: Option<ObjectId>,
        positioner: ObjectId,
    ) -> Result<(), ProtocolError> {
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        let c = self.client_mut(request);
        let Resource::Positioner =
            c.resource(request.object, positioner, &xdg_positioner::INTERFACE)?
        else {
            unreachable!("positioners are xdg_positioner objects");
        };
        let parent = match parent {
            None => None,
            Some(object) => {
                let Resource::XdgSurface(parent) =
                    c.resource(request.object, object, &xdg_surface::INTERFACE)?
                else {
                    unreachable!("xdg surfaces are xdg_surface objects");
                };
                let has_role = self
                    .surfaces
                    .get(&parent)
                    .is_some_and(|s| matches!(s.role, Role::Toplevel | Role::Popup));
                if !has_role {
                    return Err(error(
                        xdg_wm_base::error::INVALID_POPUP_PARENT,
                        "a parent that is no toplevel or popup",
                    ));
                }
                Some(parent)
            }
        };
        let Some(positioner) = self.positioner(request.client, positioner) else {
            unreachable!("a positioner object has its state");
        };
        if !positioner.is_complete() {
            return Err(error(
                xdg_wm_base::error::INVALID_POSITIONER,
                "a positioner without a size or an anchor rectangle",
            ));
        }
        let c = self.client_mut(request);
        c.create(
            id,
            &xdg_popup::INTERFACE,
            request.version,
            Resource::Popup(surface),
        )?;
        self.surface_mut(surface)?.role = Role::Popup;
        self.popups.popups.insert(
            surface,
            Popup {
                client: request.client,
                object: id.0,
                parent,
                positioner,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                dismissed: false,
                grabbed: false,
            },
        );
        Ok(())
    }

    pub(super) fn popup_request(
        &mut self,
        request: Request,
        surface: SurfaceId,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let decoded = xdg_popup::Request::decode(request.opcode, args)
            .map_err(|e| ProtocolError::invalid_method(request.object, format!("{e:?}")))?;
        let live = self
            .popups
            .popups
            .get(&surface)
            .is_some_and(|p| p.object == request.object);
        match decoded {
            xdg_popup::Request::Destroy => {
                self.client_mut(request).destroy(request.object);
                if live {
                    self.dismiss_popups_of(surface);
                    self.popups.popups.remove(&surface);
                }
            }
            xdg_popup::Request::Grab { seat, .. } => {
                let c = self.client_mut(request);
                c.resource(request.object, seat, &wl_seat::INTERFACE)?;
                let mapped = self
                    .surfaces
                    .get(&surface)
                    .is_some_and(|s| s.current.buffer.is_some());
                if live && mapped {
                    return Err(ProtocolError::new(
                        request.object,
                        xdg_popup::error::INVALID_GRAB,
                        "a grab after the popup was mapped",
                    ));
                }
                if let Some(popup) = self.popups.popups.get_mut(&surface).filter(|_| live) {
                    popup.grabbed = true;
                }
            }
            xdg_popup::Request::Reposition { positioner, token } => {
                let c = self.client_mut(request);
                c.resource(request.object, positioner, &xdg_positioner::INTERFACE)?;
                let Some(new) = self.positioner(request.client, positioner) else {
                    return Ok(());
                };
                if !new.is_complete() {
                    return Err(ProtocolError::new(
                        request.object,
                        xdg_wm_base::error::INVALID_POSITIONER,
                        "a positioner without a size or an anchor rectangle",
                    ));
                }
                if live && let Some(popup) = self.popups.popups.get_mut(&surface) {
                    popup.positioner = new;
                    let object = popup.object;
                    let c = self.client_mut(request);
                    xdg_popup::send_repositioned(&mut c.out, object, token);
                    self.configure_popup(surface);
                }
            }
        }
        Ok(())
    }

    /// The origin of the surface `id` on the output, which a window, a
    /// layer surface and a popup have.
    pub(super) fn surface_origin(&self, id: SurfaceId) -> Option<(i32, i32)> {
        let mut at = id;
        let mut origin = (0, 0);
        // A popup lies against its parent's window geometry; the chain
        // of parents is no longer than the popups there are.
        for _ in 0..=self.popups.popups.len() {
            if let Some(popup) = self.popups.popups.get(&at) {
                let parent = popup.parent?;
                let geometry = self.geometry_offset(at);
                origin = (
                    origin.0 + popup.rect.x - geometry.0,
                    origin.1 + popup.rect.y - geometry.1,
                );
                let (px, py) = self.geometry_offset(parent);
                origin = (origin.0 + px, origin.1 + py);
                at = parent;
                continue;
            }
            let base = if let Some(window) = self.windows().iter().find(|w| w.surface == at) {
                (window.frame.x, window.frame.y)
            } else {
                let extent = self.extent(self.surfaces.get(&at)?);
                let place = self.layer_place(at, extent)?;
                (place.x, place.y)
            };
            return Some((origin.0 + base.0, origin.1 + base.1));
        }
        None
    }

    /// The top left corner of the window geometry of `id` within its
    /// surface.
    fn geometry_offset(&self, id: SurfaceId) -> (i32, i32) {
        self.surfaces
            .get(&id)
            .and_then(|s| s.xdg.as_ref())
            .and_then(|x| x.geometry)
            .map_or((0, 0), |g| (g.x, g.y))
    }

    /// Places the popup of `id` against its parent, and sends its client
    /// `configure` with the rectangle, and the serial to acknowledge.
    pub(super) fn configure_popup(&mut self, id: SurfaceId) {
        let screen = self.screen.unwrap_or((0, 0));
        let Some(popup) = self.popups.popups.get(&id) else {
            return;
        };
        let Some(parent) = popup.parent else {
            return;
        };
        let (positioner, client, object) = (popup.positioner, popup.client, popup.object);
        // The output, in the coordinates of the parent's window geometry.
        let (ox, oy) = self.surface_origin(parent).unwrap_or((0, 0));
        let (gx, gy) = self.geometry_offset(parent);
        let bounds = Rect {
            x: -(ox + gx),
            y: -(oy + gy),
            width: screen.0,
            height: screen.1,
        };
        let rect = positioner.place(bounds);
        if let Some(popup) = self.popups.popups.get_mut(&id) {
            popup.rect = rect;
        }
        let serial = self.next_serial();
        let Some(xdg) = self.surfaces.get_mut(&id).and_then(|s| s.xdg.as_mut()) else {
            return;
        };
        xdg.sent.push(serial);
        let surface_object = xdg.object;
        if let Some(c) = self.clients.get_mut(&client) {
            xdg_popup::send_configure(&mut c.out, object, rect.x, rect.y, rect.width, rect.height);
            xdg_surface::send_configure(&mut c.out, surface_object, serial);
        }
    }

    /// Makes `parent` the parent of the popup `popup` that has none; says
    /// whether it had none.
    pub(super) fn set_popup_parent(&mut self, popup: SurfaceId, parent: SurfaceId) -> bool {
        match self.popups.popups.get_mut(&popup) {
            Some(p) if p.parent.is_none() => {
                p.parent = Some(parent);
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    /// The popups of `parent` that are shown: made, configured, mapped
    /// and not dismissed, oldest first.
    pub(super) fn popups_of(&self, parent: SurfaceId) -> Vec<SurfaceId> {
        let shown = |id: &SurfaceId| {
            let s = self.surfaces.get(id);
            s.is_some_and(|s| {
                s.current.buffer.is_some() && s.xdg.as_ref().is_some_and(|x| x.configured)
            })
        };
        let popups = self.popups.popups.iter();
        let mine = popups.filter(|(_, p)| p.parent == Some(parent) && !p.dismissed);
        mine.map(|(&id, _)| id).filter(shown).collect()
    }

    /// Dismisses the popups of `parent`, and theirs: each is told
    /// `popup_done` and is not shown any more.
    pub(super) fn dismiss_popups_of(&mut self, parent: SurfaceId) {
        let mut work = vec![parent];
        while let Some(at) = work.pop() {
            let children: Vec<SurfaceId> = self
                .popups
                .popups
                .iter()
                .filter(|(_, p)| p.parent == Some(at) && !p.dismissed)
                .map(|(&id, _)| id)
                .collect();
            for id in children {
                self.dismiss_popup(id);
                work.push(id);
            }
        }
    }

    /// Ends the grabs of the popups that grabbed, and are not of the client
    /// whose surface was pressed, `under`, or of no surface at all: each
    /// is dismissed with the popups below it.
    pub(super) fn dismiss_grabs_outside(&mut self, under: Option<ClientId>) {
        let outside: Vec<SurfaceId> = self
            .popups
            .popups
            .iter()
            .filter(|(_, p)| p.grabbed && !p.dismissed && Some(p.client) != under)
            .map(|(&id, _)| id)
            .collect();
        for id in outside {
            self.dismiss_popup(id);
            self.dismiss_popups_of(id);
        }
    }

    /// Tells the popup of `id` that it is done, and stops showing it.
    pub(super) fn dismiss_popup(&mut self, id: SurfaceId) {
        let Some(popup) = self.popups.popups.get_mut(&id).filter(|p| !p.dismissed) else {
            return;
        };
        popup.dismissed = true;
        let (client, object) = (popup.client, popup.object);
        if let Some(c) = self.clients.get_mut(&client) {
            xdg_popup::send_popup_done(&mut c.out, object);
        }
    }

    /// Forgets the positioners and popups of `client`, which went.
    pub(super) fn forget_client_popups(&mut self, client: ClientId) {
        self.popups.positioners.retain(|(c, _), _| *c != client);
        self.popups.popups.retain(|_, p| p.client != client);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{wl_callback, wl_compositor, wl_display, wl_registry};

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

    /// A client with the compositor 3 and xdg_wm_base 4, the toplevel
    /// surface 5 with the xdg_surface 6, mapped, and the positioner 10
    /// that places a popup of 100 by 50 below a 20 by 10 rectangle at
    /// (30, 40) of it.
    fn parent(server: &mut Server) -> (TestClient, SurfaceId) {
        server.set_screen((1280, 800));
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(
            server,
            2,
            &crate::wayland::protocols::xdg_wm_base::INTERFACE,
            4,
        );
        client.send(server, 3, 0, vec![Arg::NewId(NewId(5))]);
        client.send(
            server,
            4,
            2,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(5))],
        );
        client.send(server, 6, 1, vec![Arg::NewId(NewId(7))]);
        let id = *server.surfaces.keys().next().unwrap();
        server.buffer_sizes.insert(99, (300, 200));
        let s = server.surfaces.get_mut(&id).unwrap();
        s.current.buffer = Some(99);
        s.xdg.as_mut().unwrap().configured = true;
        client.send(server, 4, 1, vec![Arg::NewId(NewId(10))]);
        client.send(server, 10, 1, vec![Arg::Int(100), Arg::Int(50)]);
        let anchor_rect = [30, 40, 20, 10].map(Arg::Int);
        client.send(server, 10, 2, Vec::from(anchor_rect));
        client.send(server, 10, 3, vec![Arg::Uint(6)]);
        client.send(server, 10, 4, vec![Arg::Uint(8)]);
        (client, id)
    }

    /// Makes the popup 11 of the surface 8, whose xdg_surface is 9, of the
    /// xdg_surface `parent`, and makes the first commit.
    fn popup(server: &mut Server, client: &TestClient, parent: u32) {
        client.send(server, 3, 0, vec![Arg::NewId(NewId(8))]);
        client.send(
            server,
            4,
            2,
            vec![Arg::NewId(NewId(9)), Arg::Object(Some(8))],
        );
        let args = vec![
            Arg::NewId(NewId(11)),
            Arg::Object(Some(parent)),
            Arg::Object(Some(10)),
        ];
        client.send(server, 9, 2, args);
        client.send(server, 8, 6, vec![]);
    }

    fn popup_events(server: &mut Server, client: &TestClient) -> Vec<(u32, u16, Vec<Arg>)> {
        let sent = client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            9 => &xdg_surface::EVENTS,
            11 => &xdg_popup::EVENTS,
            14 => &wl_seat::EVENTS,
            _ => &wl_callback::EVENTS,
        });
        sent.into_iter().filter(|e| e.0 == 9 || e.0 == 11).collect()
    }

    #[test]
    fn the_first_commit_of_a_popup_is_answered_with_its_place_and_a_serial() {
        let mut server = Server::new();
        let (client, _) = parent(&mut server);
        popup(&mut server, &client, 6);
        assert_eq!(client.error(&mut server), None);
        let sent = popup_events(&mut server, &client);
        let kinds: Vec<(u32, u16)> = sent.iter().map(|e| (e.0, e.1)).collect();
        assert_eq!(kinds, [(11, 0), (9, 0)]);
        // Below the anchor rectangle's bottom left corner.
        assert_eq!(
            sent[0].2,
            [Arg::Int(30), Arg::Int(50), Arg::Int(100), Arg::Int(50)]
        );
    }

    #[test]
    fn a_mapped_popup_is_shown_above_its_parent_at_its_place() {
        let mut server = Server::new();
        let (client, parent) = parent(&mut server);
        popup(&mut server, &client, 6);
        let Arg::Uint(serial) = popup_events(&mut server, &client)[1].2[0] else {
            panic!("a serial");
        };
        client.send(&mut server, 9, 4, vec![Arg::Uint(serial)]);
        let id = *server.popups.popups.keys().next().unwrap();
        server.buffer_sizes.insert(98, (100, 50));
        let s = server.surfaces.get_mut(&id).unwrap();
        s.pending.buffer = Some(98);
        s.attached = true;
        client.send(&mut server, 8, 6, vec![]);
        assert_eq!(client.error(&mut server), None);
        let shown = server.shown();
        let ids: Vec<SurfaceId> = shown.iter().map(|s| s.surface).collect();
        assert_eq!(ids, [parent, id]);
        assert_eq!((shown[1].x - shown[0].x, shown[1].y - shown[0].y), (30, 50));
        assert!(shown.iter().all(|s| s.root == parent));
        // A pointer over the popup is in the popup.
        server.pointer_motion(f64::from(shown[1].x + 5), f64::from(shown[1].y + 5), 1);
        assert_eq!(server.pointer_surface(), Some(id));
    }

    #[test]
    fn refuses_a_popup_without_a_complete_positioner_or_a_parent_with_a_role() {
        let mut server = Server::new();
        let (client, _) = parent(&mut server);
        client.send(&mut server, 4, 1, vec![Arg::NewId(NewId(12))]);
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(8))]);
        client.send(
            &mut server,
            4,
            2,
            vec![Arg::NewId(NewId(9)), Arg::Object(Some(8))],
        );
        let args = vec![
            Arg::NewId(NewId(11)),
            Arg::Object(Some(6)),
            Arg::Object(Some(12)),
        ];
        client.send(&mut server, 9, 2, args);
        let code = xdg_wm_base::error::INVALID_POSITIONER;
        assert_eq!(client.error(&mut server), Some(code));
        // A parent that is an xdg_surface without a role.
        let mut server = Server::new();
        let (client, _) = parent(&mut server);
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(13))]);
        client.send(
            &mut server,
            4,
            2,
            vec![Arg::NewId(NewId(14)), Arg::Object(Some(13))],
        );
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(8))]);
        client.send(
            &mut server,
            4,
            2,
            vec![Arg::NewId(NewId(9)), Arg::Object(Some(8))],
        );
        let args = vec![
            Arg::NewId(NewId(11)),
            Arg::Object(Some(14)),
            Arg::Object(Some(10)),
        ];
        client.send(&mut server, 9, 2, args);
        let code = xdg_wm_base::error::INVALID_POPUP_PARENT;
        assert_eq!(client.error(&mut server), Some(code));
    }

    #[test]
    fn a_parent_that_goes_dismisses_its_popups_and_a_reposition_places_again() {
        let mut server = Server::new();
        let (client, _) = parent(&mut server);
        popup(&mut server, &client, 6);
        popup_events(&mut server, &client);
        // A reposition with another positioner is answered with the token
        // and a new place.
        client.send(&mut server, 4, 1, vec![Arg::NewId(NewId(12))]);
        client.send(&mut server, 12, 1, vec![Arg::Int(10), Arg::Int(10)]);
        client.send(&mut server, 12, 2, Vec::from([0, 0, 5, 5].map(Arg::Int)));
        client.send(
            &mut server,
            11,
            2,
            vec![Arg::Object(Some(12)), Arg::Uint(77)],
        );
        let sent = popup_events(&mut server, &client);
        let kinds: Vec<(u32, u16)> = sent.iter().map(|e| (e.0, e.1)).collect();
        assert_eq!(kinds, [(11, 2), (11, 0), (9, 0)]);
        assert_eq!(sent[0].2, [Arg::Uint(77)]);
        assert_eq!(client.error(&mut server), None);
        // The parent's surface is destroyed: the popup is done.
        client.send(&mut server, 5, 0, vec![]);
        let sent = popup_events(&mut server, &client);
        assert!(sent.iter().any(|e| (e.0, e.1) == (11, 1)));
    }

    #[test]
    fn a_press_outside_the_client_of_a_popup_that_grabbed_dismisses_it() {
        let mut server = Server::new();
        let (client, _) = parent(&mut server);
        client.bind(
            &mut server,
            2,
            &crate::wayland::protocols::wl_seat::INTERFACE,
            14,
        );
        popup_with_grab(&mut server, &client);
        let sent = popup_events(&mut server, &client);
        assert!(!sent.iter().any(|e| (e.0, e.1) == (11, 1)));
        // A press on a surface of its own client leaves it.
        let window = *server.surfaces.keys().next().unwrap();
        server.move_window(window, 0, 0);
        server.pointer_motion(5.0, 5.0, 1);
        server.pointer_button(272, true, 2);
        server.pointer_button(272, false, 3);
        assert!(
            !popup_events(&mut server, &client)
                .iter()
                .any(|e| (e.0, e.1) == (11, 1))
        );
        // A press on no surface ends the grab.
        server.pointer_motion(1200.0, 700.0, 4);
        server.pointer_button(272, true, 5);
        assert!(
            popup_events(&mut server, &client)
                .iter()
                .any(|e| (e.0, e.1) == (11, 1))
        );
    }

    /// Makes the popup 11 of the surface 8 with a grab, which the first
    /// commit comes after.
    fn popup_with_grab(server: &mut Server, client: &TestClient) {
        client.send(server, 3, 0, vec![Arg::NewId(NewId(8))]);
        client.send(
            server,
            4,
            2,
            vec![Arg::NewId(NewId(9)), Arg::Object(Some(8))],
        );
        let args = vec![
            Arg::NewId(NewId(11)),
            Arg::Object(Some(6)),
            Arg::Object(Some(10)),
        ];
        client.send(server, 9, 2, args);
        client.send(server, 11, 1, vec![Arg::Object(Some(14)), Arg::Uint(1)]);
        client.send(server, 8, 6, vec![]);
    }

    #[test]
    fn a_grab_after_the_popup_was_mapped_is_an_error() {
        let mut server = Server::new();
        let (client, _) = parent(&mut server);
        client.bind(
            &mut server,
            2,
            &crate::wayland::protocols::wl_seat::INTERFACE,
            14,
        );
        popup(&mut server, &client, 6);
        let id = *server.popups.popups.keys().next().unwrap();
        server.surfaces.get_mut(&id).unwrap().current.buffer = Some(98);
        client.send(
            &mut server,
            11,
            1,
            vec![Arg::Object(Some(14)), Arg::Uint(1)],
        );
        assert_eq!(
            client.error(&mut server),
            Some(xdg_popup::error::INVALID_GRAB)
        );
    }
}
