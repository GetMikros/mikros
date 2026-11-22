//! `wl_seat` and `wl_pointer` of the core protocol (MIKSPEC-0066.R16): the
//! one seat, with a pointer and, once it has a keymap, a keyboard
//! (`keyboard`), and the pointer's focus. The program moves
//! the pointer and presses its buttons; the surface under it, the topmost
//! window or panel whose input region holds it, gets `enter`, `motion`,
//! `button` and `axis` events in its own coordinates, and `leave` when
//! the pointer leaves it. While a button is held, the surface that got
//! the press keeps the pointer. A client chooses the cursor of its
//! surfaces with `set_cursor` or a shape of `wp_cursor_shape_device_v1`
//! (MIKSPEC-0066.R28), for the serial of the last `enter` it got.

use super::area::Area;
use super::{ClientId, Imports, ProtocolError, Request, Resource, Role, Server, SurfaceId};
use crate::wayland::protocols::{wl_keyboard, wl_pointer, wl_seat, wl_surface};
use crate::wayland::wire::{Arg, Fixed, ObjectId};

/// The version of `wl_seat` that the compositor offers, and the first
/// versions of `wl_pointer` with `frame` and `axis_source`, and with
/// `axis_value120` in place of `axis_discrete`.
pub const SEAT_VERSION: u32 = 8;
const FRAMES: u32 = 5;
const VALUE120: u32 = 8;

/// The shape `default` of cursor-shape.
pub const DEFAULT_SHAPE: u32 = 1;

/// The distance that one notch of a wheel scrolls, in surface units.
const NOTCH: f64 = 15.0;

/// The cursor that a client chose for its surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    /// A shape of cursor-shape.
    Shape(u32),
    /// Its surface, whose hotspot lies at the pointer.
    Surface {
        surface: SurfaceId,
        hotspot: (i32, i32),
    },
    Hidden,
}

impl Default for Cursor {
    fn default() -> Cursor {
        Cursor::Shape(DEFAULT_SHAPE)
    }
}

/// The surface that has the pointer, its client and object, and where its
/// corner lies on the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Focus {
    surface: SurfaceId,
    client: ClientId,
    object: ObjectId,
    origin: (i32, i32),
}

/// The pointer: its position on the output, the surface that has it, the
/// buttons held, and the serial of the last `enter` each client got.
#[derive(Default)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    focus: Option<Focus>,
    pressed: Vec<u32>,
    entered: std::collections::BTreeMap<ClientId, u32>,
    /// The clients that got events since the last frame.
    framed: Vec<ClientId>,
    /// The window that the pointer moves, if one is.
    pub(super) moving: Option<super::grabs::Move>,
}

impl Server {
    pub(super) fn seat_request(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::Seat => match wl_seat::Request::decode(request.opcode, args)
                .map_err(invalid)?
            {
                wl_seat::Request::GetPointer { id } => {
                    let c = self.client_mut(request);
                    c.create(
                        id,
                        &wl_pointer::INTERFACE,
                        request.version,
                        Resource::Pointer,
                    )?;
                    // A pointer made while the client's surface has the
                    // pointer learns so at once.
                    if let Some(focus) = self.pointer.focus.filter(|f| f.client == request.client) {
                        let serial = self.pointer.entered.get(&request.client).copied();
                        if let Some(serial) = serial {
                            self.enter_one(request.client, id.0, serial, focus);
                        }
                    }
                    Ok(())
                }
                wl_seat::Request::GetKeyboard { id } => self.get_keyboard(imports, request, id),
                wl_seat::Request::GetTouch { .. } => Err(ProtocolError::new(
                    request.object,
                    wl_seat::error::MISSING_CAPABILITY,
                    "the seat has no touch screen",
                )),
                wl_seat::Request::Release => {
                    self.client_mut(request).destroy(request.object);
                    Ok(())
                }
            },
            Resource::Pointer => {
                match wl_pointer::Request::decode(request.opcode, args).map_err(invalid)? {
                    wl_pointer::Request::SetCursor {
                        serial,
                        surface,
                        hotspot_x,
                        hotspot_y,
                    } => self.set_cursor(request, serial, surface, (hotspot_x, hotspot_y)),
                    wl_pointer::Request::Release => {
                        self.client_mut(request).destroy(request.object);
                        Ok(())
                    }
                }
            }
            Resource::Keyboard => {
                match wl_keyboard::Request::decode(request.opcode, args).map_err(invalid)? {
                    wl_keyboard::Request::Release => {
                        self.client_mut(request).destroy(request.object);
                        Ok(())
                    }
                }
            }
            _ => unreachable!("seat_request handles the seat, pointers and keyboards"),
        }
    }

    /// Sends a new seat object `seat` of `version` its capabilities and
    /// name.
    pub(super) fn describe_seat(&mut self, client: ClientId, seat: ObjectId, version: u32) {
        let capabilities = self.capabilities();
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        wl_seat::send_capabilities(&mut c.out, seat, capabilities);
        if version >= 2 {
            wl_seat::send_name(&mut c.out, seat, "seat0");
        }
    }

    fn set_cursor(
        &mut self,
        request: Request,
        serial: u32,
        surface: Option<ObjectId>,
        hotspot: (i32, i32),
    ) -> Result<(), ProtocolError> {
        // A request for an older enter is ignored.
        if self.pointer.entered.get(&request.client) != Some(&serial) {
            return Ok(());
        }
        let Some(surface) = surface else {
            self.client_mut(request).cursor = Cursor::Hidden;
            return Ok(());
        };
        let c = self.client_mut(request);
        let Resource::Surface(id) = c.resource(request.object, surface, &wl_surface::INTERFACE)?
        else {
            unreachable!("surfaces are wl_surface objects");
        };
        let s = self.surface_mut(id)?;
        if !matches!(s.role, Role::None | Role::Cursor) || s.xdg.is_some() {
            return Err(ProtocolError::new(
                request.object,
                wl_pointer::error::ROLE,
                "a cursor surface that has another role",
            ));
        }
        s.role = Role::Cursor;
        self.client_mut(request).cursor = Cursor::Surface {
            surface: id,
            hotspot,
        };
        Ok(())
    }

    /// The cursor to draw: that which the client of the surface under the
    /// pointer chose, or the default shape over no surface.
    pub fn cursor(&self) -> Cursor {
        let chosen = self
            .pointer
            .focus
            .and_then(|f| self.clients.get(&f.client))
            .map(|c| c.cursor);
        match chosen {
            // A cursor surface that is gone shows the default.
            Some(Cursor::Surface { surface, .. }) if !self.surfaces.contains_key(&surface) => {
                Cursor::default()
            }
            Some(cursor) => cursor,
            None => Cursor::default(),
        }
    }

    /// Makes the shape `shape` of cursor-shape the cursor of `client`,
    /// for the serial `serial`.
    pub(super) fn set_cursor_shape(&mut self, client: ClientId, serial: u32, shape: u32) {
        if self.pointer.entered.get(&client) == Some(&serial)
            && let Some(c) = self.clients.get_mut(&client)
        {
            c.cursor = Cursor::Shape(shape);
        }
    }

    /// The topmost window or panel whose input region holds `(x, y)`, and
    /// its corner.
    fn surface_at(&self, x: i32, y: i32) -> Option<Focus> {
        self.shown().into_iter().rev().find_map(|shown| {
            let (width, height) = *self.buffer_sizes.get(&shown.buffer)?;
            let (lx, ly) = (x - shown.x, y - shown.y);
            if lx < 0 || ly < 0 || lx >= width as i32 || ly >= height as i32 {
                return None;
            }
            let s = self.surfaces.get(&shown.surface)?;
            if let Some(input) = &s.current.input
                && !Area::of(input).contains(lx, ly)
            {
                return None;
            }
            Some(Focus {
                surface: shown.surface,
                client: s.client,
                object: s.object,
                origin: (shown.x, shown.y),
            })
        })
    }

    /// The objects of `client`'s pointers, with their versions.
    fn pointers_of(&self, client: ClientId) -> Vec<(ObjectId, u32)> {
        self.objects_of(client, Resource::Pointer)
    }

    /// The objects of `client` that are `resource`, with their versions.
    pub(super) fn objects_of(&self, client: ClientId, resource: Resource) -> Vec<(ObjectId, u32)> {
        self.clients.get(&client).map_or_else(Vec::new, |c| {
            c.objects
                .iter()
                .filter(|(_, o)| o.resource == resource)
                .map(|(&id, o)| (id, o.version))
                .collect()
        })
    }

    fn enter_one(&mut self, client: ClientId, pointer: ObjectId, serial: u32, focus: Focus) {
        let (x, y) = self.local(focus);
        if let Some(c) = self.clients.get_mut(&client) {
            wl_pointer::send_enter(&mut c.out, pointer, serial, focus.object, x, y);
            self.pointer.framed.push(client);
        }
    }

    /// The pointer's position in the surface of `focus`.
    fn local(&self, focus: Focus) -> (Fixed, Fixed) {
        let x = self.pointer.x - f64::from(focus.origin.0);
        let y = self.pointer.y - f64::from(focus.origin.1);
        (Fixed::from_f64(x), Fixed::from_f64(y))
    }

    /// Gives the pointer to the surface under it, sending `leave` to the
    /// one that had it and `enter` to the new one, unless a button is held
    /// or the surface under it has it already. A surface that is gone
    /// loses it without `leave`.
    pub fn refocus(&mut self) {
        self.focus_new_windows();
        self.keep_keyboard_focus();
        self.update_toplevel_lists();
        if let Some(focus) = self.pointer.focus
            && !self.surfaces.contains_key(&focus.surface)
        {
            self.pointer.focus = None;
            self.pointer.pressed.clear();
        }
        if !self.pointer.pressed.is_empty() {
            return;
        }
        let under = self.surface_at(self.pointer.x as i32, self.pointer.y as i32);
        let old = self.pointer.focus;
        if under.map(|f| f.surface) == old.map(|f| f.surface) {
            self.pointer.focus = under;
            return;
        }
        if let Some(old) = old {
            let serial = self.next_serial();
            for (pointer, _) in self.pointers_of(old.client) {
                if let Some(c) = self.clients.get_mut(&old.client) {
                    wl_pointer::send_leave(&mut c.out, pointer, serial, old.object);
                }
            }
            self.pointer.framed.push(old.client);
        }
        self.pointer.focus = under;
        if let Some(new) = under {
            let serial = self.next_serial();
            self.pointer.entered.insert(new.client, serial);
            for (pointer, _) in self.pointers_of(new.client) {
                self.enter_one(new.client, pointer, serial, new);
            }
        }
    }

    /// The button that was pressed last and is held, if one is.
    pub(super) fn button_held(&self) -> Option<u32> {
        self.pointer.pressed.last().copied()
    }

    /// Whether the surface `surface` has the pointer.
    pub(super) fn pointer_in(&self, surface: SurfaceId) -> bool {
        self.pointer.focus.is_some_and(|f| f.surface == surface)
    }

    /// Takes the pointer from the surface that has it, with `leave`.
    pub(super) fn leave_pointer_focus(&mut self) {
        let Some(old) = self.pointer.focus.take() else {
            return;
        };
        let serial = self.next_serial();
        for (pointer, _) in self.pointers_of(old.client) {
            if let Some(c) = self.clients.get_mut(&old.client) {
                wl_pointer::send_leave(&mut c.out, pointer, serial, old.object);
            }
        }
        self.pointer.framed.push(old.client);
    }

    /// Moves the pointer to `(x, y)` on the output, kept on it, at the
    /// time `time` in milliseconds.
    pub fn pointer_motion(&mut self, x: f64, y: f64, time: u32) {
        let (width, height) = self.screen.unwrap_or((1, 1));
        self.pointer.x = x.clamp(0.0, f64::from(width.max(1) - 1));
        self.pointer.y = y.clamp(0.0, f64::from(height.max(1) - 1));
        // A window that is moved follows the pointer, which no client sees.
        if self.move_with_pointer() {
            return;
        }
        self.refocus();
        let Some(focus) = self.pointer.focus else {
            return;
        };
        let (lx, ly) = self.local(focus);
        for (pointer, _) in self.pointers_of(focus.client) {
            if let Some(c) = self.clients.get_mut(&focus.client) {
                wl_pointer::send_motion(&mut c.out, pointer, time, lx, ly);
            }
        }
        self.pointer.framed.push(focus.client);
    }

    /// Presses or releases the button `button`, an evdev code, at `time`.
    pub fn pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        let held = self.pointer.pressed.contains(&button);
        match (pressed, held) {
            (true, false) => self.pointer.pressed.push(button),
            (false, true) => self.pointer.pressed.retain(|&b| b != button),
            _ => return,
        }
        // The release of the button that moves a window ends the move.
        if !pressed && self.end_move(button) {
            self.refocus();
            return;
        }
        let Some(focus) = self.pointer.focus else {
            return;
        };
        // A press on a window gives it the keyboard.
        let window = self.surfaces.get(&focus.surface).map(|s| s.role) == Some(Role::Toplevel);
        if pressed && window {
            self.raise(focus.surface);
            self.focus_keyboard(Some(focus.surface));
            // Super and the primary button move the window, whose client
            // does not get the press.
            if button == super::grabs::BTN_LEFT && self.super_held() {
                self.start_move(focus.surface, button);
                return;
            }
        }
        let serial = self.next_serial();
        if pressed {
            self.record_press(focus.client, serial);
        }
        let state = match pressed {
            true => wl_pointer::button_state::PRESSED,
            false => wl_pointer::button_state::RELEASED,
        };
        for (pointer, _) in self.pointers_of(focus.client) {
            if let Some(c) = self.clients.get_mut(&focus.client) {
                wl_pointer::send_button(&mut c.out, pointer, serial, time, button, state);
            }
        }
        self.pointer.framed.push(focus.client);
        // The last release ends the press's hold on the surface.
        if self.pointer.pressed.is_empty() {
            self.refocus();
        }
    }

    /// Scrolls by `vertical` and `horizontal` notches of a wheel, positive
    /// down and right, at `time`.
    pub fn pointer_axis(&mut self, vertical: i32, horizontal: i32, time: u32) {
        let Some(focus) = self.pointer.focus else {
            return;
        };
        let axes = [
            (wl_pointer::axis::VERTICAL_SCROLL, vertical),
            (wl_pointer::axis::HORIZONTAL_SCROLL, horizontal),
        ];
        for (pointer, version) in self.pointers_of(focus.client) {
            let Some(c) = self.clients.get_mut(&focus.client) else {
                continue;
            };
            for (axis, notches) in axes.into_iter().filter(|&(_, n)| n != 0) {
                if version >= FRAMES {
                    let source = wl_pointer::axis_source::WHEEL;
                    wl_pointer::send_axis_source(&mut c.out, pointer, source);
                }
                if version >= VALUE120 {
                    wl_pointer::send_axis_value120(&mut c.out, pointer, axis, notches * 120);
                } else if version >= FRAMES {
                    wl_pointer::send_axis_discrete(&mut c.out, pointer, axis, notches);
                }
                let value = Fixed::from_f64(f64::from(notches) * NOTCH);
                wl_pointer::send_axis(&mut c.out, pointer, time, axis, value);
            }
        }
        self.pointer.framed.push(focus.client);
    }

    /// Ends the group of pointer events since the last, with `frame` to
    /// each client that got some.
    pub fn pointer_frame(&mut self) {
        let mut clients = std::mem::take(&mut self.pointer.framed);
        clients.sort_unstable();
        clients.dedup();
        for client in clients {
            for (pointer, version) in self.pointers_of(client) {
                if version >= FRAMES
                    && let Some(c) = self.clients.get_mut(&client)
                {
                    wl_pointer::send_frame(&mut c.out, pointer);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_registry};
    use crate::wayland::wire::NewId;

    /// A client with registry 2, compositor 3, seat 4, pointer 5 and the
    /// surface 6, a toplevel of 100 by 50 at the output's corner.
    fn with_window(server: &mut Server) -> TestClient {
        server.set_screen((1280, 800));
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &wl_seat::INTERFACE, 4);
        client.send(server, 4, 0, vec![Arg::NewId(NewId(5))]);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(6))]);
        let (&id, surface) = server.surfaces.iter_mut().next().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        server.move_window(id, 0, 0);
        client
    }

    fn pointer_events(server: &mut Server, client: &TestClient) -> Vec<(u32, u16, Vec<Arg>)> {
        client
            .events(server, &|id| match id {
                4 => &wl_seat::EVENTS,
                5 => &wl_pointer::EVENTS,
                _ => &wl_registry::EVENTS,
            })
            .into_iter()
            .filter(|e| e.0 == 5)
            .collect()
    }

    const ENTER: u16 = 0;
    const LEAVE: u16 = 1;
    const MOTION: u16 = 2;
    const BUTTON: u16 = 3;
    const AXIS: u16 = 4;
    const FRAME: u16 = 5;

    #[test]
    fn tells_clients_the_seat_has_a_pointer() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        let events = client.events(&mut server, &|id| match id {
            4 => &wl_seat::EVENTS,
            _ => &wl_registry::EVENTS,
        });
        let capabilities = events.iter().find(|e| (e.0, e.1) == (4, 0)).unwrap();
        assert_eq!(capabilities.2, [Arg::Uint(wl_seat::capability::POINTER)]);
        assert!(events.iter().any(|e| (e.0, e.1) == (4, 1)));
        client.send(&mut server, 4, 1, vec![Arg::NewId(NewId(7))]);
        let missing = wl_seat::error::MISSING_CAPABILITY;
        assert_eq!(client.error(&mut server), Some(missing));
    }

    #[test]
    fn enters_moves_and_leaves_the_surface_under_the_pointer() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        pointer_events(&mut server, &client);
        server.pointer_motion(10.0, 20.0, 7);
        server.pointer_frame();
        let events = pointer_events(&mut server, &client);
        let kinds: Vec<u16> = events.iter().map(|e| e.1).collect();
        assert_eq!(kinds, [ENTER, MOTION, FRAME]);
        assert_eq!(
            events[0].2[1..],
            [
                Arg::Object(Some(6)),
                Arg::Fixed(Fixed(2560)),
                Arg::Fixed(Fixed(5120))
            ]
        );
        server.pointer_motion(500.0, 20.0, 8);
        server.pointer_frame();
        let kinds: Vec<u16> = pointer_events(&mut server, &client)
            .iter()
            .map(|e| e.1)
            .collect();
        assert_eq!(kinds, [LEAVE, FRAME]);
        // Off the output's edge, the pointer stays on it.
        server.pointer_motion(-50.0, 900.0, 9);
        assert_eq!((server.pointer.x, server.pointer.y), (0.0, 799.0));
    }

    #[test]
    fn a_click_raises_the_window_under_the_pointer() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        let below = *server.surfaces.keys().next().unwrap();
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(8))]);
        let above = *server.surfaces.keys().last().unwrap();
        let surface = server.surfaces.get_mut(&above).unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(98);
        server.buffer_sizes.insert(98, (100, 50));
        server.windows();
        server.move_window(above, 50, 0);
        // The second window lies over the right half of the first.
        let top = |server: &Server| server.surface_at(75, 10).map(|f| f.surface);
        assert_eq!(top(&server), Some(above));
        server.pointer_motion(10.0, 10.0, 1);
        server.pointer_button(0x110, true, 2);
        server.pointer_button(0x110, false, 3);
        assert_eq!(top(&server), Some(below));
    }

    /// The surface of the window that `with_window` made, its frame's corner.
    fn corner(server: &Server) -> (i32, i32) {
        let window = server.windows().into_iter().next().unwrap();
        (window.frame.x, window.frame.y)
    }

    fn button_events(server: &mut Server, client: &TestClient) -> usize {
        pointer_events(server, client)
            .iter()
            .filter(|e| e.1 == BUTTON)
            .count()
    }

    #[test]
    fn super_and_the_primary_button_move_the_window_without_its_client() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        pointer_events(&mut server, &client);
        server.pointer_motion(10.0, 10.0, 1);
        server.keyboard_key(125, true, 2, Default::default());
        server.pointer_button(0x110, true, 3);
        assert_eq!(button_events(&mut server, &client), 0);
        server.pointer_motion(110.0, 60.0, 4);
        assert_eq!(corner(&server), (100, 50));
        server.pointer_button(0x110, false, 5);
        assert_eq!(button_events(&mut server, &client), 0);
        // After the move the pointer is a pointer again: the window is
        // where it was put, and clicks reach its client.
        server.keyboard_key(125, false, 6, Default::default());
        server.pointer_motion(120.0, 70.0, 7);
        server.pointer_button(0x110, true, 8);
        assert_eq!(button_events(&mut server, &client), 1);
        assert_eq!(corner(&server), (100, 50));
    }

    #[test]
    fn the_primary_button_without_super_does_not_move_the_window() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        pointer_events(&mut server, &client);
        server.pointer_motion(10.0, 10.0, 1);
        server.pointer_button(0x110, true, 3);
        server.pointer_motion(110.0, 60.0, 4);
        assert_eq!(corner(&server), (0, 0));
        assert_eq!(button_events(&mut server, &client), 1);
    }

    #[test]
    fn xdg_toplevel_move_follows_the_press_that_it_names() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        let surface = *server.surfaces.keys().next().unwrap();
        pointer_events(&mut server, &client);
        server.pointer_motion(10.0, 10.0, 1);
        server.pointer_button(0x110, true, 2);
        let serial = server.activation.last_press().unwrap();
        // A serial that is not of a press that it got does nothing.
        server.request_move(client.id, surface, serial + 100);
        server.pointer_motion(60.0, 40.0, 3);
        assert_eq!(corner(&server), (0, 0));
        // The pointer holds the window where it is, at (60, 40).
        server.request_move(client.id, surface, serial);
        server.pointer_motion(110.0, 60.0, 4);
        assert_eq!(corner(&server), (50, 20));
        server.pointer_button(0x110, false, 5);
        server.pointer_motion(0.0, 0.0, 6);
        assert_eq!(corner(&server), (50, 20));
    }

    #[test]
    fn keeps_the_surface_of_a_press_until_the_release() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        server.pointer_motion(10.0, 10.0, 1);
        server.pointer_button(0x110, true, 2);
        server.pointer_motion(300.0, 300.0, 3);
        server.pointer_frame();
        let kinds: Vec<u16> = pointer_events(&mut server, &client)
            .iter()
            .map(|e| e.1)
            .collect();
        assert_eq!(kinds, [ENTER, MOTION, BUTTON, MOTION, FRAME]);
        server.pointer_button(0x110, false, 4);
        server.pointer_frame();
        let kinds: Vec<u16> = pointer_events(&mut server, &client)
            .iter()
            .map(|e| e.1)
            .collect();
        assert_eq!(kinds, [BUTTON, LEAVE, FRAME]);
    }

    #[test]
    fn scrolls_in_notches() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        server.pointer_motion(10.0, 10.0, 1);
        pointer_events(&mut server, &client);
        server.pointer_axis(2, 0, 5);
        server.pointer_frame();
        let events = pointer_events(&mut server, &client);
        let axis = events.iter().find(|e| e.1 == AXIS).unwrap();
        let vertical = wl_pointer::axis::VERTICAL_SCROLL;
        assert_eq!(
            axis.2[1..],
            [Arg::Uint(vertical), Arg::Fixed(Fixed(30 * 256))]
        );
    }

    #[test]
    fn takes_the_cursor_of_the_client_under_the_pointer() {
        let mut server = Server::new();
        let client = with_window(&mut server);
        assert_eq!(server.cursor(), Cursor::Shape(DEFAULT_SHAPE));
        server.pointer_motion(10.0, 10.0, 1);
        let serial = match &pointer_events(&mut server, &client)[0].2[0] {
            Arg::Uint(serial) => *serial,
            other => panic!("{other:?}"),
        };
        // An older serial is ignored, the last one hides the cursor.
        client.send(
            &mut server,
            5,
            0,
            vec![
                Arg::Uint(serial - 1),
                Arg::Object(None),
                Arg::Int(0),
                Arg::Int(0),
            ],
        );
        assert_eq!(server.cursor(), Cursor::Shape(DEFAULT_SHAPE));
        client.send(
            &mut server,
            5,
            0,
            vec![
                Arg::Uint(serial),
                Arg::Object(None),
                Arg::Int(0),
                Arg::Int(0),
            ],
        );
        assert_eq!(server.cursor(), Cursor::Hidden);
        // A surface of its own as the cursor; the window cannot be one.
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(7))]);
        client.send(
            &mut server,
            5,
            0,
            vec![
                Arg::Uint(serial),
                Arg::Object(Some(7)),
                Arg::Int(3),
                Arg::Int(4),
            ],
        );
        let Cursor::Surface { hotspot, .. } = server.cursor() else {
            panic!("a cursor surface");
        };
        assert_eq!(hotspot, (3, 4));
        client.send(
            &mut server,
            5,
            0,
            vec![
                Arg::Uint(serial),
                Arg::Object(Some(6)),
                Arg::Int(0),
                Arg::Int(0),
            ],
        );
        assert_eq!(client.error(&mut server), Some(wl_pointer::error::ROLE));
        // Away from the window, the default shape.
        server.pointer_motion(600.0, 600.0, 2);
        assert_eq!(server.cursor(), Cursor::Shape(DEFAULT_SHAPE));
    }
}
