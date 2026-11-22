//! `zwlr_layer_shell_v1` (MIKSPEC-0076.R24): a surface with the role of a
//! layer surface lies on an output at the edges that it is anchored to,
//! in a layer, and asks for an exclusive zone, which the work area leaves
//! out (MIKSPEC-0074.R29), and for keyboard focus. Its state is double
//! buffered with `wl_surface.commit`; the compositor answers the first
//! commit, and every commit that changes the size that the surface gets,
//! with `configure`, and the surface is shown only once its client
//! acknowledged one and attached a buffer. In the layers `background` and
//! `bottom` the exclusive zone counts as 0 and the keyboard interactivity
//! as `none`.

use super::{ClientId, ProtocolError, Rect, Request, Resource, Role, Server, SurfaceId};
use crate::wayland::protocols::{
    wl_output, wl_surface, zwlr_layer_shell_v1 as shell, zwlr_layer_surface_v1 as surface_v1,
};
use crate::wayland::wire::{Arg, NewId, ObjectId};
use std::collections::BTreeMap;

pub const VERSION: u32 = 5;

const LAYER_BACKGROUND: u32 = 0;
const LAYER_BOTTOM: u32 = 1;
const LAYER_OVERLAY: u32 = 3;
const ANCHOR_TOP: u32 = 1;
const ANCHOR_BOTTOM: u32 = 2;
const ANCHOR_LEFT: u32 = 4;
const ANCHOR_RIGHT: u32 = 8;
const ANCHOR_ALL: u32 = 15;
const KEYBOARD_ON_DEMAND: u32 = 2;

/// The state that requests set and a commit applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub layer: u32,
    /// The size that the client asked for, 0 where the compositor picks.
    pub size: (u32, u32),
    pub anchor: u32,
    pub zone: i32,
    /// The margins, top, right, bottom and left.
    pub margin: [i32; 4],
    pub interactivity: u32,
    /// The edge for the exclusive zone, or 0 to deduce it.
    pub edge: u32,
}

impl Default for State {
    fn default() -> State {
        State {
            layer: LAYER_BACKGROUND,
            size: (0, 0),
            anchor: 0,
            zone: 0,
            margin: [0; 4],
            interactivity: 0,
            edge: 0,
        }
    }
}

impl State {
    /// The size that the surface gets on an output of `output`: the
    /// client's, or in an axis where it asked for 0 and is anchored to
    /// both edges, what the margins leave. An axis where it asked for 0
    /// without that is invalid, and gets 0.
    pub fn size_on(&self, (width, height): (i32, i32)) -> (u32, u32) {
        let [top, right, bottom, left] = self.margin;
        let axis = |asked: u32, both: bool, extent: i32, near: i32, far: i32| match (asked, both) {
            (0, true) => (extent - near - far).max(0) as u32,
            (asked, _) => asked,
        };
        let horizontal = self.anchor & (ANCHOR_LEFT | ANCHOR_RIGHT) == ANCHOR_LEFT | ANCHOR_RIGHT;
        let vertical = self.anchor & (ANCHOR_TOP | ANCHOR_BOTTOM) == ANCHOR_TOP | ANCHOR_BOTTOM;
        (
            axis(self.size.0, horizontal, width, left, right),
            axis(self.size.1, vertical, height, top, bottom),
        )
    }

    /// Whether an axis asked for 0 without an anchor to both of its edges.
    fn size_is_invalid(&self) -> bool {
        let horizontal = self.anchor & (ANCHOR_LEFT | ANCHOR_RIGHT) == ANCHOR_LEFT | ANCHOR_RIGHT;
        let vertical = self.anchor & (ANCHOR_TOP | ANCHOR_BOTTOM) == ANCHOR_TOP | ANCHOR_BOTTOM;
        (self.size.0 == 0 && !horizontal) || (self.size.1 == 0 && !vertical)
    }

    /// The place of a surface of `size` on an output of `output`, which
    /// its anchors and margins decide: against an edge that it is anchored
    /// to alone, and centred in an axis where it is anchored to both or
    /// neither.
    pub fn place(&self, (width, height): (i32, i32), output: (i32, i32)) -> Rect {
        let [top, right, bottom, left] = self.margin;
        let axis = |near: bool, far: bool, size: i32, extent: i32, m: (i32, i32)| match (near, far)
        {
            (true, false) => m.0,
            (false, true) => extent - size - m.1,
            (true, true) => m.0 + (extent - m.0 - m.1 - size) / 2,
            (false, false) => (extent - size) / 2,
        };
        Rect {
            x: axis(
                self.anchor & ANCHOR_LEFT != 0,
                self.anchor & ANCHOR_RIGHT != 0,
                width,
                output.0,
                (left, right),
            ),
            y: axis(
                self.anchor & ANCHOR_TOP != 0,
                self.anchor & ANCHOR_BOTTOM != 0,
                height,
                output.1,
                (top, bottom),
            ),
            width,
            height,
        }
    }

    /// The edge that the exclusive zone applies to: the one that was set,
    /// or the only anchored one, or the one that the other two anchored
    /// edges of three are perpendicular to.
    pub fn exclusive_edge(&self) -> Option<u32> {
        if self.edge != 0 {
            return Some(self.edge);
        }
        let anchor = self.anchor & ANCHOR_ALL;
        match anchor.count_ones() {
            1 => Some(anchor),
            3 => [
                (ANCHOR_TOP, ANCHOR_BOTTOM),
                (ANCHOR_BOTTOM, ANCHOR_TOP),
                (ANCHOR_LEFT, ANCHOR_RIGHT),
                (ANCHOR_RIGHT, ANCHOR_LEFT),
            ]
            .into_iter()
            .find(|&(edge, opposite)| anchor & edge != 0 && anchor & opposite == 0)
            .map(|(edge, _)| edge)
            .or_else(|| {
                // Three edges leave out one edge, whose opposite is the one.
                [ANCHOR_TOP, ANCHOR_BOTTOM, ANCHOR_LEFT, ANCHOR_RIGHT]
                    .into_iter()
                    .find(|&e| anchor & e == 0)
                    .map(|missing| match missing {
                        ANCHOR_TOP => ANCHOR_BOTTOM,
                        ANCHOR_BOTTOM => ANCHOR_TOP,
                        ANCHOR_LEFT => ANCHOR_RIGHT,
                        _ => ANCHOR_LEFT,
                    })
            }),
            _ => None,
        }
    }

    /// The part of the output that the surface reserves, as the edge and
    /// its depth from it, the zone and the margin on that edge, or none:
    /// the zone is not above 0, the edge is not known, or the layer is
    /// `background` or `bottom`.
    pub fn reserved(&self) -> Option<(u32, i32)> {
        if self.zone <= 0 || self.layer <= LAYER_BOTTOM {
            return None;
        }
        let edge = self.exclusive_edge()?;
        let [top, right, bottom, left] = self.margin;
        let margin = match edge {
            ANCHOR_TOP => top,
            ANCHOR_BOTTOM => bottom,
            ANCHOR_LEFT => left,
            _ => right,
        };
        Some((edge, self.zone.saturating_add(margin)))
    }

    /// The keyboard interactivity that the compositor honours.
    pub fn keyboard(&self) -> u32 {
        if self.layer <= LAYER_BOTTOM {
            0
        } else {
            self.interactivity
        }
    }
}

/// A layer surface.
#[derive(Debug)]
pub struct Layer {
    pub client: ClientId,
    pub object: ObjectId,
    pub namespace: String,
    pub pending: State,
    pub current: State,
    /// The serial and size of every configure that is not acknowledged
    /// yet, and whether one was acknowledged.
    sent: Vec<(u32, (u32, u32))>,
    pub configured: bool,
    /// Whether a commit asked for the first configure, and the size of
    /// the last one sent.
    started: bool,
    last: Option<(u32, u32)>,
    /// Set when the compositor closed it: it is not served any more.
    pub closed: bool,
    /// When it was mapped, for the order of surfaces in a layer.
    pub mapped_at: Option<u64>,
}

/// The layer surfaces of every surface that has one.
#[derive(Debug, Default)]
pub(super) struct Layers {
    pub(super) surfaces: BTreeMap<SurfaceId, Layer>,
    mapped: u64,
}

impl Layers {
    /// The reservations of the layer surfaces that are mapped, as the edge
    /// and the depth.
    pub(super) fn reservations(&self) -> Vec<(u32, i32)> {
        let shown = self
            .surfaces
            .values()
            .filter(|l| l.configured && !l.closed && l.mapped_at.is_some());
        shown.filter_map(|l| l.current.reserved()).collect()
    }
}

/// The edges of `reserved`, which `Server::work_area` takes off.
pub(super) fn reserve(area: &mut Rect, edge: u32, depth: i32) {
    let depth = depth.max(0);
    match edge {
        ANCHOR_TOP => {
            area.y = area.y.saturating_add(depth);
            area.height = area.height.saturating_sub(depth);
        }
        ANCHOR_BOTTOM => area.height = area.height.saturating_sub(depth),
        ANCHOR_LEFT => {
            area.x = area.x.saturating_add(depth);
            area.width = area.width.saturating_sub(depth);
        }
        _ => area.width = area.width.saturating_sub(depth),
    }
}

impl Server {
    pub(super) fn layer_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::LayerShell => {
                match shell::Request::decode(request.opcode, args).map_err(invalid)? {
                    shell::Request::Destroy => self.client_mut(request).destroy(request.object),
                    shell::Request::GetLayerSurface {
                        id,
                        surface,
                        output,
                        layer,
                        namespace,
                    } => self.get_layer_surface(request, id, surface, output, layer, namespace)?,
                }
                Ok(())
            }
            Resource::LayerSurface(surface) => {
                let decoded = surface_v1::Request::decode(request.opcode, args).map_err(invalid)?;
                self.layer_surface_request(request, surface, decoded)
            }
            _ => unreachable!("layer_request handles layer surfaces"),
        }
    }

    fn get_layer_surface(
        &mut self,
        request: Request,
        id: NewId,
        surface: ObjectId,
        output: Option<ObjectId>,
        layer: u32,
        namespace: String,
    ) -> Result<(), ProtocolError> {
        let c = self.client_mut(request);
        let Resource::Surface(surface_id) =
            c.resource(request.object, surface, &wl_surface::INTERFACE)?
        else {
            unreachable!("surfaces are wl_surface objects");
        };
        if let Some(output) = output {
            c.resource(request.object, output, &wl_output::INTERFACE)?;
        }
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        if layer > LAYER_OVERLAY {
            return Err(error(
                shell::error::INVALID_LAYER,
                "a layer that is not 0 to 3",
            ));
        }
        let s = self.surface_mut(surface_id)?;
        let free = match s.role {
            Role::None => s.xdg.is_none() && s.panel.is_none(),
            Role::Layer => !self.layers.surfaces.contains_key(&surface_id),
            _ => false,
        };
        if !free {
            return Err(error(shell::error::ROLE, "a surface that has another role"));
        }
        let s = self.surface_mut(surface_id)?;
        if s.current.buffer.is_some() || s.pending.buffer.is_some() || !s.waiting.is_empty() {
            return Err(error(
                shell::error::ALREADY_CONSTRUCTED,
                "a surface that has a buffer",
            ));
        }
        s.role = Role::Layer;
        let c = self.client_mut(request);
        let resource = Resource::LayerSurface(surface_id);
        c.create(id, &surface_v1::INTERFACE, request.version, resource)?;
        let state = State {
            layer,
            ..State::default()
        };
        self.layers.surfaces.insert(
            surface_id,
            Layer {
                client: request.client,
                object: id.0,
                namespace,
                pending: state,
                current: state,
                sent: Vec::new(),
                configured: false,
                started: false,
                last: None,
                closed: false,
                mapped_at: None,
            },
        );
        Ok(())
    }

    fn layer_surface_request(
        &mut self,
        request: Request,
        surface: SurfaceId,
        decoded: surface_v1::Request,
    ) -> Result<(), ProtocolError> {
        let live = self
            .layers
            .surfaces
            .get(&surface)
            .is_some_and(|l| l.object == request.object);
        if matches!(decoded, surface_v1::Request::Destroy) {
            self.client_mut(request).destroy(request.object);
            if live {
                self.layers.surfaces.remove(&surface);
            }
            return Ok(());
        }
        let error = |code, message: &str| ProtocolError::new(request.object, code, message);
        let Some(layer) = self
            .layers
            .surfaces
            .get_mut(&surface)
            .filter(|l| l.object == request.object && !l.closed)
        else {
            return Ok(());
        };
        let pending = &mut layer.pending;
        match decoded {
            surface_v1::Request::Destroy => unreachable!("handled above"),
            surface_v1::Request::SetSize { width, height } => pending.size = (width, height),
            surface_v1::Request::SetAnchor { anchor } => {
                if anchor > ANCHOR_ALL {
                    return Err(error(
                        surface_v1::error::INVALID_ANCHOR,
                        "an anchor with a bit that is not an edge",
                    ));
                }
                pending.anchor = anchor;
            }
            surface_v1::Request::SetExclusiveZone { zone } => pending.zone = zone,
            surface_v1::Request::SetMargin {
                top,
                right,
                bottom,
                left,
            } => pending.margin = [top, right, bottom, left],
            surface_v1::Request::SetKeyboardInteractivity {
                keyboard_interactivity,
            } => {
                let known = keyboard_interactivity < KEYBOARD_ON_DEMAND
                    || (keyboard_interactivity == KEYBOARD_ON_DEMAND && request.version >= 4);
                if !known {
                    return Err(error(
                        surface_v1::error::INVALID_KEYBOARD_INTERACTIVITY,
                        "an interactivity that the version does not have",
                    ));
                }
                pending.interactivity = keyboard_interactivity;
            }
            surface_v1::Request::SetLayer { layer: value } => {
                if value > LAYER_OVERLAY {
                    return Err(ProtocolError::new(
                        request.object,
                        shell::error::INVALID_LAYER,
                        "a layer that is not 0 to 3",
                    ));
                }
                pending.layer = value;
            }
            surface_v1::Request::SetExclusiveEdge { edge } => {
                if edge != 0 && (edge.count_ones() != 1 || edge > ANCHOR_RIGHT) {
                    return Err(error(
                        surface_v1::error::INVALID_EXCLUSIVE_EDGE,
                        "an edge that is not one of the four",
                    ));
                }
                pending.edge = edge;
            }
            surface_v1::Request::AckConfigure { serial } => {
                let Some(at) = layer.sent.iter().position(|&(s, _)| s == serial) else {
                    return Err(error(
                        surface_v1::error::INVALID_SURFACE_STATE,
                        "a serial that no configure sent",
                    ));
                };
                layer.sent.drain(..=at);
                layer.configured = true;
            }
            surface_v1::Request::GetPopup { .. } => {
                return Err(error(
                    surface_v1::error::INVALID_SURFACE_STATE,
                    "popups are not served yet",
                ));
            }
        }
        Ok(())
    }

    /// Checks a commit of the surface `id` against its layer surface: no
    /// buffer before a configure was acknowledged, no size of 0 without
    /// anchors to both edges, no exclusive edge that it is not anchored to.
    pub(super) fn check_layer_commit(&self, id: SurfaceId) -> Result<(), ProtocolError> {
        let Some(layer) = self.layers.surfaces.get(&id).filter(|l| !l.closed) else {
            return Ok(());
        };
        let error = |code, message: &str| ProtocolError::new(layer.object, code, message);
        let s = &self.surfaces[&id];
        if !layer.configured && s.pending.buffer.is_some() {
            return Err(error(
                surface_v1::error::INVALID_SURFACE_STATE,
                "a buffer before a configure was acknowledged",
            ));
        }
        let pending = &layer.pending;
        if pending.size_is_invalid() {
            return Err(error(
                surface_v1::error::INVALID_SIZE,
                "a size of 0 without anchors to both edges of its axis",
            ));
        }
        if pending.edge != 0 && pending.anchor & pending.edge == 0 {
            return Err(error(
                surface_v1::error::INVALID_EXCLUSIVE_EDGE,
                "an exclusive edge that the surface is not anchored to",
            ));
        }
        Ok(())
    }

    /// Applies the state of the layer surface of `id` after a commit, and
    /// sends `configure` for the first commit and for a size that is not
    /// the last one sent; a surface with a buffer, after a configure was
    /// acknowledged, is mapped.
    pub(super) fn layer_committed(&mut self, id: SurfaceId) {
        let screen = self.screen.unwrap_or((0, 0));
        let serial = self.serial.wrapping_add(1);
        let mapped = self
            .surfaces
            .get(&id)
            .is_some_and(|s| s.current.buffer.is_some());
        let Some(layer) = self.layers.surfaces.get_mut(&id).filter(|l| !l.closed) else {
            return;
        };
        layer.current = layer.pending;
        if mapped && layer.configured {
            if layer.mapped_at.is_none() {
                self.layers.mapped += 1;
                layer.mapped_at = Some(self.layers.mapped);
            }
        } else {
            layer.mapped_at = None;
        }
        let size = layer.current.size_on(screen);
        if layer.started && layer.last == Some(size) {
            return;
        }
        layer.started = true;
        layer.last = Some(size);
        layer.sent.push((serial, size));
        let (object, client) = (layer.object, layer.client);
        self.serial = serial;
        if let Some(c) = self.clients.get_mut(&client) {
            surface_v1::send_configure(&mut c.out, object, serial, size.0, size.1);
        }
    }

    /// Closes the layer surface of `id`: its client is told, and it is
    /// not shown or served any more.
    pub(super) fn close_layer(&mut self, id: SurfaceId) {
        let Some(layer) = self.layers.surfaces.get_mut(&id).filter(|l| !l.closed) else {
            return;
        };
        layer.closed = true;
        layer.mapped_at = None;
        let (object, client) = (layer.object, layer.client);
        if let Some(c) = self.clients.get_mut(&client) {
            surface_v1::send_closed(&mut c.out, object);
        }
    }

    /// Forgets the layer surfaces of `client`, which went.
    pub(super) fn forget_client_layers(&mut self, client: ClientId) {
        self.layers.surfaces.retain(|_, l| l.client != client);
    }

    /// The layer surface of `id` and its place on the output, when it is
    /// shown: a configure was acknowledged, it has a buffer, and it is
    /// not closed.
    pub(super) fn layer_place(&self, id: SurfaceId, size: (i32, i32)) -> Option<Rect> {
        let layer = self.layers.surfaces.get(&id)?;
        if !layer.configured || layer.closed {
            return None;
        }
        Some(layer.current.place(size, self.screen.unwrap_or((0, 0))))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{wl_callback, wl_compositor, wl_display, wl_registry};

    fn state(anchor: u32, size: (u32, u32)) -> State {
        State {
            anchor,
            size,
            layer: 2,
            ..State::default()
        }
    }

    #[test]
    fn sizes_and_places_a_surface_by_its_anchors_and_margins() {
        let output = (1280, 800);
        // A bar along the top: anchored to three edges, as wide as the output.
        let bar = State {
            margin: [10, 20, 0, 30],
            ..state(ANCHOR_TOP | ANCHOR_LEFT | ANCHOR_RIGHT, (0, 40))
        };
        assert_eq!(bar.size_on(output), (1230, 40));
        let place = bar.place((1230, 40), output);
        assert_eq!((place.x, place.y), (30, 10));
        // Smaller than the margins leave, it is centred in that axis.
        assert_eq!(bar.place((1000, 40), output).x, 30 + (1230 - 1000) / 2);
        // A corner, a centred edge, and no anchor.
        let corner = state(ANCHOR_BOTTOM | ANCHOR_RIGHT, (100, 50));
        let place = corner.place((100, 50), output);
        assert_eq!((place.x, place.y), (1180, 750));
        let edge = state(ANCHOR_LEFT, (100, 50));
        let place = edge.place((100, 50), output);
        assert_eq!((place.x, place.y), (0, 375));
        let free = state(0, (100, 50));
        let place = free.place((100, 50), output);
        assert_eq!((place.x, place.y), (590, 375));
        // 0 without both anchors of its axis is invalid.
        assert!(state(ANCHOR_LEFT, (0, 50)).size_is_invalid());
        assert!(!bar.size_is_invalid());
    }

    #[test]
    fn deduces_the_exclusive_edge_and_leaves_out_what_the_layer_does_not_reserve() {
        let zone = |anchor, layer, zone| State {
            zone,
            layer,
            margin: [3, 5, 7, 11],
            ..state(anchor, (0, 0))
        };
        assert_eq!(zone(ANCHOR_TOP, 2, 40).reserved(), Some((ANCHOR_TOP, 43)));
        assert_eq!(zone(ANCHOR_LEFT, 3, 40).reserved(), Some((ANCHOR_LEFT, 51)));
        let three = ANCHOR_BOTTOM | ANCHOR_LEFT | ANCHOR_RIGHT;
        assert_eq!(zone(three, 2, 40).reserved(), Some((ANCHOR_BOTTOM, 47)));
        // A corner needs an edge, which can be set.
        let corner = ANCHOR_TOP | ANCHOR_LEFT;
        assert_eq!(zone(corner, 2, 40).reserved(), None);
        let set = State {
            edge: ANCHOR_LEFT,
            ..zone(corner, 2, 40)
        };
        assert_eq!(set.reserved(), Some((ANCHOR_LEFT, 51)));
        // Not above 0, and in the layers below the windows.
        assert_eq!(zone(ANCHOR_TOP, 2, 0).reserved(), None);
        assert_eq!(zone(ANCHOR_TOP, 2, -1).reserved(), None);
        assert_eq!(zone(ANCHOR_TOP, 1, 40).reserved(), None);
        assert_eq!(zone(ANCHOR_TOP, 0, 40).reserved(), None);
        let mut area = Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
        };
        reserve(&mut area, ANCHOR_TOP, 43);
        reserve(&mut area, ANCHOR_RIGHT, 10);
        assert_eq!(
            (area.x, area.y, area.width, area.height),
            (0, 43, 1270, 757)
        );
    }

    /// A client with the compositor 3, the layer shell 4 and the surface 5.
    fn connect(server: &mut Server) -> TestClient {
        server.set_screen((1280, 800));
        server.add_global(&shell::INTERFACE, VERSION, Resource::LayerShell);
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &shell::INTERFACE, 4);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(5))]);
        client
    }

    /// Makes the layer surface 7 of the surface 5, in `layer`.
    fn get(server: &mut Server, client: &TestClient, layer: u32) {
        let args = vec![
            Arg::NewId(NewId(7)),
            Arg::Object(Some(5)),
            Arg::Object(None),
            Arg::Uint(layer),
            Arg::String(Some("test".into())),
        ];
        client.send(server, 4, 0, args);
    }

    fn events(server: &mut Server, client: &TestClient) -> Vec<(u16, Vec<Arg>)> {
        let sent = client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            7 => &surface_v1::EVENTS,
            _ => &wl_callback::EVENTS,
        });
        sent.into_iter()
            .filter(|e| e.0 == 7)
            .map(|e| (e.1, e.2))
            .collect()
    }

    fn commit(server: &mut Server, client: &TestClient) {
        client.send(server, 5, 6, vec![]);
    }

    #[test]
    fn answers_the_first_commit_with_a_configure_and_another_when_the_size_changes() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 0, vec![Arg::Uint(0), Arg::Uint(40)]);
        client.send(
            &mut server,
            7,
            1,
            vec![Arg::Uint(ANCHOR_ALL & !ANCHOR_BOTTOM)],
        );
        assert!(events(&mut server, &client).is_empty());
        commit(&mut server, &client);
        let sent = events(&mut server, &client);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1[1..], [Arg::Uint(1280), Arg::Uint(40)]);
        // The same size is not sent again, a new one is.
        commit(&mut server, &client);
        assert!(events(&mut server, &client).is_empty());
        client.send(&mut server, 7, 0, vec![Arg::Uint(0), Arg::Uint(60)]);
        commit(&mut server, &client);
        let sent = events(&mut server, &client);
        assert_eq!(sent[0].1[1..], [Arg::Uint(1280), Arg::Uint(60)]);
        assert_eq!(client.error(&mut server), None);
    }

    #[test]
    fn refuses_what_the_protocol_does() {
        // A layer that does not exist.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 4);
        assert_eq!(client.error(&mut server), Some(shell::error::INVALID_LAYER));
        // A surface that has another role.
        let mut server = Server::new();
        let client = connect(&mut server);
        let id = *server.surfaces.keys().next().unwrap();
        server.surfaces.get_mut(&id).unwrap().role = Role::Toplevel;
        get(&mut server, &client, 2);
        assert_eq!(client.error(&mut server), Some(shell::error::ROLE));
        // A surface that has a buffer.
        let mut server = Server::new();
        let client = connect(&mut server);
        let id = *server.surfaces.keys().next().unwrap();
        server.surfaces.get_mut(&id).unwrap().current.buffer = Some(1);
        get(&mut server, &client, 2);
        assert_eq!(
            client.error(&mut server),
            Some(shell::error::ALREADY_CONSTRUCTED)
        );
        // A size of 0 without anchors to both edges, at the commit.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 0, vec![Arg::Uint(0), Arg::Uint(40)]);
        commit(&mut server, &client);
        assert_eq!(
            client.error(&mut server),
            Some(surface_v1::error::INVALID_SIZE)
        );
        // An anchor with a bit that is not an edge.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 1, vec![Arg::Uint(16)]);
        assert_eq!(
            client.error(&mut server),
            Some(surface_v1::error::INVALID_ANCHOR)
        );
        // An exclusive edge that it is not anchored to.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 0, vec![Arg::Uint(10), Arg::Uint(10)]);
        client.send(&mut server, 7, 1, vec![Arg::Uint(ANCHOR_LEFT)]);
        client.send(&mut server, 7, 9, vec![Arg::Uint(ANCHOR_TOP)]);
        commit(&mut server, &client);
        let code = surface_v1::error::INVALID_EXCLUSIVE_EDGE;
        assert_eq!(client.error(&mut server), Some(code));
        // A serial that no configure sent.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 6, vec![Arg::Uint(99)]);
        let code = surface_v1::error::INVALID_SURFACE_STATE;
        assert_eq!(client.error(&mut server), Some(code));
    }

    #[test]
    fn a_buffer_before_the_configure_is_acknowledged_is_an_error_and_after_it_maps() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 0, vec![Arg::Uint(100), Arg::Uint(50)]);
        commit(&mut server, &client);
        events(&mut server, &client);
        let id = *server.surfaces.keys().next().unwrap();
        server.buffer_sizes.insert(1, (100, 50));
        server.surfaces.get_mut(&id).unwrap().pending.buffer = Some(1);
        server.surfaces.get_mut(&id).unwrap().attached = true;
        commit(&mut server, &client);
        let code = surface_v1::error::INVALID_SURFACE_STATE;
        assert_eq!(client.error(&mut server), Some(code));
        // Acknowledged, the commit maps it, centred.
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        client.send(&mut server, 7, 0, vec![Arg::Uint(100), Arg::Uint(50)]);
        commit(&mut server, &client);
        let Arg::Uint(serial) = events(&mut server, &client)[0].1[0] else {
            panic!("a serial");
        };
        client.send(&mut server, 7, 6, vec![Arg::Uint(serial)]);
        let id = *server.surfaces.keys().next().unwrap();
        server.buffer_sizes.insert(1, (100, 50));
        server.surfaces.get_mut(&id).unwrap().pending.buffer = Some(1);
        server.surfaces.get_mut(&id).unwrap().attached = true;
        commit(&mut server, &client);
        assert_eq!(client.error(&mut server), None);
        let place = server.layer_place(id, (100, 50)).unwrap();
        assert_eq!((place.x, place.y), (590, 375));
        assert!(server.layers.surfaces[&id].mapped_at.is_some());
    }

    #[test]
    fn a_destroyed_layer_surface_keeps_the_role_and_a_closed_one_is_not_served() {
        let mut server = Server::new();
        let client = connect(&mut server);
        get(&mut server, &client, 2);
        let id = *server.surfaces.keys().next().unwrap();
        server.close_layer(id);
        let sent = events(&mut server, &client);
        assert_eq!(sent.iter().filter(|e| e.0 == 1).count(), 1);
        client.send(&mut server, 7, 0, vec![Arg::Uint(1), Arg::Uint(1)]);
        assert_eq!(server.layers.surfaces[&id].pending.size, (0, 0));
        client.send(&mut server, 7, 7, vec![]);
        assert!(server.layers.surfaces.is_empty());
        assert_eq!(server.surfaces[&id].role, Role::Layer);
        // It can have a layer surface again.
        get(&mut server, &client, 3);
        assert_eq!(client.error(&mut server), None);
    }

    /// Makes the surface `surface` (a new one) a layer surface `object` in
    /// `layer`, anchored as `anchor`, of `size`, with the exclusive zone
    /// `zone`, and maps it with a buffer of that size.
    fn map(
        server: &mut Server,
        client: &TestClient,
        surface: u32,
        object: u32,
        layer: u32,
        (anchor, size, zone): (u32, (u32, u32), i32),
    ) -> SurfaceId {
        client.send(server, 3, 0, vec![Arg::NewId(NewId(surface))]);
        let args = vec![
            Arg::NewId(NewId(object)),
            Arg::Object(Some(surface)),
            Arg::Object(None),
            Arg::Uint(layer),
            Arg::String(Some("test".into())),
        ];
        client.send(server, 4, 0, args);
        client.send(
            server,
            object,
            0,
            vec![Arg::Uint(size.0), Arg::Uint(size.1)],
        );
        client.send(server, object, 1, vec![Arg::Uint(anchor)]);
        client.send(server, object, 2, vec![Arg::Int(zone)]);
        client.send(server, surface, 6, vec![]);
        let sent = client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            id if id >= 10 => &surface_v1::EVENTS,
            _ => &wl_callback::EVENTS,
        });
        let configure = sent.iter().find(|e| e.0 == object).unwrap();
        let Arg::Uint(serial) = configure.2[0] else {
            panic!("a serial");
        };
        client.send(server, object, 6, vec![Arg::Uint(serial)]);
        let id = *server
            .surfaces
            .iter()
            .find(|(_, s)| s.object == surface)
            .unwrap()
            .0;
        let buffer = u64::from(surface);
        server
            .buffer_sizes
            .insert(buffer, (size.0.max(10), size.1.max(10)));
        let s = server.surfaces.get_mut(&id).unwrap();
        s.pending.buffer = Some(buffer);
        s.attached = true;
        client.send(server, surface, 6, vec![]);
        id
    }

    const TOP_BAR: (u32, (u32, u32), i32) = (ANCHOR_TOP | ANCHOR_LEFT | ANCHOR_RIGHT, (0, 40), 40);

    #[test]
    fn stacks_the_layers_around_the_windows_and_later_mapped_surfaces_above() {
        let mut server = Server::new();
        let client = connect(&mut server);
        let window = *server.surfaces.keys().next().unwrap();
        {
            let s = server.surfaces.get_mut(&window).unwrap();
            s.role = Role::Toplevel;
            s.current.buffer = Some(99);
            server.buffer_sizes.insert(99, (100, 50));
        }
        let overlay = map(&mut server, &client, 20, 30, 3, (0, (50, 50), 0));
        let top_a = map(&mut server, &client, 21, 31, 2, (0, (50, 50), 0));
        let background = map(&mut server, &client, 22, 32, 0, (0, (50, 50), 0));
        let bottom = map(&mut server, &client, 23, 33, 1, (0, (50, 50), 0));
        let top_b = map(&mut server, &client, 24, 34, 2, (0, (50, 50), 0));
        assert_eq!(client.error(&mut server), None);
        let order: Vec<SurfaceId> = server.shown().iter().map(|s| s.surface).collect();
        assert_eq!(order, [background, bottom, window, top_a, top_b, overlay]);
    }

    #[test]
    fn an_active_fullscreen_window_lies_between_the_top_and_the_overlay_layers() {
        let mut server = Server::new();
        let client = connect(&mut server);
        let window = *server.surfaces.keys().next().unwrap();
        {
            let s = server.surfaces.get_mut(&window).unwrap();
            s.role = Role::Toplevel;
            s.current.buffer = Some(99);
            s.xdg = Some(super::super::xdg::XdgState::windowed(None, false));
            s.xdg.as_mut().unwrap().fullscreen = true;
            server.buffer_sizes.insert(99, (100, 50));
        }
        let top = map(&mut server, &client, 21, 31, 2, (0, (50, 50), 0));
        let overlay = map(&mut server, &client, 20, 30, 3, (0, (50, 50), 0));
        let order = |server: &Server| -> Vec<SurfaceId> {
            server.shown().iter().map(|s| s.surface).collect()
        };
        // Without the focus the window is one of the others.
        assert_eq!(order(&server), [window, top, overlay]);
        server.focus_keyboard(Some(window));
        // With it, it covers the layer `top`.
        assert_eq!(order(&server), [top, window, overlay]);
    }

    #[test]
    fn the_exclusive_zones_of_layers_above_the_windows_leave_the_work_area() {
        let mut server = Server::new();
        let client = connect(&mut server);
        let full = server.work_area();
        assert_eq!((full.width, full.height), (1280, 800));
        map(&mut server, &client, 20, 30, 2, TOP_BAR);
        let area = server.work_area();
        assert_eq!(
            (area.x, area.y, area.width, area.height),
            (0, 40, 1280, 760)
        );
        // Zones on one edge add up, and layers below the windows have none.
        map(&mut server, &client, 21, 31, 3, TOP_BAR);
        map(
            &mut server,
            &client,
            22,
            32,
            1,
            (ANCHOR_LEFT | ANCHOR_TOP | ANCHOR_BOTTOM, (30, 0), 30),
        );
        let area = server.work_area();
        assert_eq!(
            (area.x, area.y, area.width, area.height),
            (0, 80, 1280, 720)
        );
        // A closed one leaves it free again.
        let id = *server.layers.surfaces.keys().next().unwrap();
        server.close_layer(id);
        let area = server.work_area();
        assert_eq!(area.y, 40);
    }
}
