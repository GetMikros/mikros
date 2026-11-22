//! `wl_keyboard` of the core protocol (MIKSPEC-0066.R16): the seat's
//! keyboard and the window that has its focus. Each keyboard gets the
//! keymap that the program compiled, as a memory object of its own
//! (MIKSPEC-0066.R6), and again whenever the program compiles another
//! (MIKSPEC-0066.R29), and then the key repeat, as every keyboard of version
//! 4 or later does again whenever it changes (MIKSPEC-0066.R29). The
//! client of the focused window gets `enter` with
//! the keys held and the modifiers, then `key` and `modifiers`, and
//! `leave` when the focus moves on. A press of a pointer button on a
//! window gives it the focus (MIKSPEC-0074.R6); when the focused window
//! is gone, the topmost window that is left gets it, or none does.

use super::{ClientId, Imports, ProtocolError, Request, Resource, Role, Server, SurfaceId};
use crate::keyboard::Repeat;
use crate::wayland::protocols::{wl_keyboard, wl_seat};
use crate::wayland::wire::{NewId, ObjectId};

/// The modifiers as `wl_keyboard.modifiers` gives them: those held,
/// latched and locked, and the layout in effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    pub group: u32,
}

/// The window that has the keyboard focus, its client and object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Focus {
    surface: SurfaceId,
    client: ClientId,
    object: ObjectId,
}

/// The first version of `wl_keyboard` with `repeat_info`.
const REPEAT_INFO: u32 = 4;

/// The keyboard: its keymap in xkb's text format, ending with a zero
/// byte, once the program gave one, its key repeat, the window that has
/// its focus, the keys held and the modifiers.
#[derive(Default)]
pub struct Keyboard {
    keymap: Option<Vec<u8>>,
    repeat: Repeat,
    focus: Option<Focus>,
    pressed: Vec<u32>,
    modifiers: Modifiers,
    /// The keys whose press was a shortcut of the compositor, which no
    /// client gets the release of.
    pub(super) swallowed: std::collections::BTreeSet<u32>,
}

impl Server {
    /// Makes a new keyboard `id` of `request`'s client and sends it the
    /// keymap, unless no memory object can be made for it, and `enter`
    /// when the client's window has the focus.
    pub(super) fn get_keyboard(
        &mut self,
        imports: &mut dyn Imports,
        request: Request,
        id: NewId,
    ) -> Result<(), ProtocolError> {
        let Some(keymap) = &self.keyboard.keymap else {
            return Err(ProtocolError::new(
                request.object,
                wl_seat::error::MISSING_CAPABILITY,
                "the seat has no keyboard",
            ));
        };
        let memory = imports.memory(keymap);
        let size = keymap.len() as u32;
        let Repeat { rate, delay } = self.keyboard.repeat;
        let c = self.client_mut(request);
        c.create(
            id,
            &wl_keyboard::INTERFACE,
            request.version,
            Resource::Keyboard,
        )?;
        if let Some(fd) = memory {
            let format = wl_keyboard::keymap_format::XKB_V1;
            wl_keyboard::send_keymap(&mut c.out, id.0, format, fd, size);
        }
        if request.version >= REPEAT_INFO {
            wl_keyboard::send_repeat_info(&mut c.out, id.0, rate as i32, delay as i32);
        }
        if let Some(focus) = self.keyboard.focus.filter(|f| f.client == request.client) {
            let serial = self.next_serial();
            self.enter_keyboard(focus, id.0, serial);
        }
        Ok(())
    }

    /// The capabilities of the seat: a pointer, and a keyboard once it
    /// has a keymap.
    pub(super) fn capabilities(&self) -> u32 {
        match self.keyboard.keymap {
            Some(_) => wl_seat::capability::POINTER | wl_seat::capability::KEYBOARD,
            None => wl_seat::capability::POINTER,
        }
    }

    /// Gives the seat the keymap `keymap`, in xkb's text format and
    /// ending with a zero byte, with the `modifiers` of its state. With
    /// the first, it tells the clients' seats that it has a keyboard;
    /// with another, every keyboard gets it in a memory object of its own
    /// that `imports` makes, and the focused window's client's keyboards
    /// get the modifiers.
    pub fn set_keymap(&mut self, imports: &mut dyn Imports, keymap: Vec<u8>, modifiers: Modifiers) {
        self.keyboard.modifiers = modifiers;
        let old = self.keyboard.keymap.replace(keymap);
        if old.is_some() {
            if old != self.keyboard.keymap {
                self.send_keymap(imports);
            }
            return;
        }
        let capabilities = self.capabilities();
        for c in self.clients.values_mut() {
            let seats = c
                .objects
                .iter()
                .filter(|(_, o)| o.resource == Resource::Seat);
            for (&seat, _) in seats {
                wl_seat::send_capabilities(&mut c.out, seat, capabilities);
            }
        }
    }

    fn send_keymap(&mut self, imports: &mut dyn Imports) {
        let Some(keymap) = self.keyboard.keymap.clone() else {
            return;
        };
        let clients: Vec<ClientId> = self.clients.keys().copied().collect();
        for client in clients {
            for (keyboard, _) in self.keyboards_of(client) {
                let (Some(fd), Some(c)) = (imports.memory(&keymap), self.clients.get_mut(&client))
                else {
                    continue;
                };
                let format = wl_keyboard::keymap_format::XKB_V1;
                wl_keyboard::send_keymap(&mut c.out, keyboard, format, fd, keymap.len() as u32);
            }
        }
        let Some(focus) = self.keyboard.focus else {
            return;
        };
        let serial = self.next_serial();
        let m = self.keyboard.modifiers;
        for (keyboard, _) in self.keyboards_of(focus.client) {
            if let Some(c) = self.clients.get_mut(&focus.client) {
                let (depressed, latched, locked) = (m.depressed, m.latched, m.locked);
                wl_keyboard::send_modifiers(
                    &mut c.out, keyboard, serial, depressed, latched, locked, m.group,
                );
            }
        }
    }

    /// The keys held, as evdev codes.
    pub fn keys_held(&self) -> Vec<u32> {
        self.keyboard.pressed.clone()
    }

    /// Sets the key repeat to `rate` keys per second after `delay`
    /// milliseconds, and sends it to the keyboards that have `repeat_info`
    /// when it changed, which it returns.
    pub fn set_repeat(&mut self, rate: u32, delay: u32) -> bool {
        let repeat = Repeat { rate, delay };
        if repeat == self.keyboard.repeat {
            return false;
        }
        self.keyboard.repeat = repeat;
        for c in self.clients.values_mut() {
            let keyboards = c
                .objects
                .iter()
                .filter(|(_, o)| o.resource == Resource::Keyboard && o.version >= REPEAT_INFO);
            for (&keyboard, _) in keyboards {
                wl_keyboard::send_repeat_info(&mut c.out, keyboard, rate as i32, delay as i32);
            }
        }
        true
    }

    /// The keyboards of `client`, with their versions.
    fn keyboards_of(&self, client: ClientId) -> Vec<(ObjectId, u32)> {
        self.objects_of(client, Resource::Keyboard)
    }

    fn enter_keyboard(&mut self, focus: Focus, keyboard: ObjectId, serial: u32) {
        let keys: Vec<u8> = self
            .keyboard
            .pressed
            .iter()
            .flat_map(|k| k.to_le_bytes())
            .collect();
        let m = self.keyboard.modifiers;
        if let Some(c) = self.clients.get_mut(&focus.client) {
            wl_keyboard::send_enter(&mut c.out, keyboard, serial, focus.object, &keys);
            let (depressed, latched, locked) = (m.depressed, m.latched, m.locked);
            wl_keyboard::send_modifiers(
                &mut c.out, keyboard, serial, depressed, latched, locked, m.group,
            );
        }
    }

    /// Gives the keyboard focus to the window `surface`, or to none,
    /// with `leave` to the window that had it and `enter` to the new one.
    pub(super) fn focus_keyboard(&mut self, surface: Option<SurfaceId>) {
        let new = surface.and_then(|id| {
            let s = self.surfaces.get(&id)?;
            Some(Focus {
                surface: id,
                client: s.client,
                object: s.object,
            })
        });
        if let Some(new) = new {
            self.remember_focus(new.surface);
        }
        let old = self.keyboard.focus;
        if new == old {
            return;
        }
        self.keyboard.focus = new;
        if let Some(new) = new {
            self.clear_attention(new.surface);
        }
        if let Some(old) = old.filter(|old| self.surfaces.contains_key(&old.surface)) {
            self.tell_inhibitor(old.surface, false);
            let serial = self.next_serial();
            for (keyboard, _) in self.keyboards_of(old.client) {
                if let Some(c) = self.clients.get_mut(&old.client) {
                    wl_keyboard::send_leave(&mut c.out, keyboard, serial, old.object);
                }
            }
        }
        if let Some(new) = new {
            // The selection comes before the focus.
            self.announce_selection_to(new.client);
            self.tell_inhibitor(new.surface, true);
            let serial = self.next_serial();
            for (keyboard, _) in self.keyboards_of(new.client) {
                self.enter_keyboard(new, keyboard, serial);
            }
        }
    }

    /// Whether the key `key`, an evdev code, is held.
    pub(super) fn keyboard_pressed(&self, key: u32) -> bool {
        self.keyboard.pressed.contains(&key)
    }

    /// The window that has the keyboard focus.
    pub fn keyboard_focus(&self) -> Option<SurfaceId> {
        self.keyboard.focus.map(|f| f.surface)
    }

    /// Moves the keyboard focus from a window that is no longer shown to
    /// the topmost window that is.
    pub(super) fn keep_keyboard_focus(&mut self) {
        let Some(focus) = self.keyboard.focus else {
            return;
        };
        let shown = self.shown();
        let windows = shown.iter().filter(|s| {
            self.surfaces
                .get(&s.surface)
                .is_some_and(|s| s.role == Role::Toplevel)
        });
        let windows: Vec<SurfaceId> = windows.map(|s| s.surface).collect();
        if !windows.contains(&focus.surface) {
            self.focus_keyboard(windows.last().copied());
        }
    }

    /// Presses or releases the key `key`, an evdev code, at `time`, and
    /// sends the focused window's client the key and then `modifiers`,
    /// when they changed to them.
    pub fn keyboard_key(&mut self, key: u32, pressed: bool, time: u32, modifiers: Modifiers) {
        let held = self.keyboard.pressed.contains(&key);
        match (pressed, held) {
            (true, false) => self.keyboard.pressed.push(key),
            (false, true) => self.keyboard.pressed.retain(|&k| k != key),
            _ => {}
        }
        let changed = modifiers != self.keyboard.modifiers;
        self.keyboard.modifiers = modifiers;
        // The shortcuts of the compositor are not for clients.
        if self.shortcut(key, pressed) {
            return;
        }
        let Some(focus) = self.keyboard.focus else {
            return;
        };
        let state = match pressed {
            true => wl_keyboard::key_state::PRESSED,
            false => wl_keyboard::key_state::RELEASED,
        };
        let serial = self.next_serial();
        if pressed {
            self.record_press(focus.client, serial);
        }
        let m = modifiers;
        for (keyboard, _) in self.keyboards_of(focus.client) {
            let Some(c) = self.clients.get_mut(&focus.client) else {
                continue;
            };
            wl_keyboard::send_key(&mut c.out, keyboard, serial, time, key, state);
            if changed {
                let (depressed, latched, locked) = (m.depressed, m.latched, m.locked);
                wl_keyboard::send_modifiers(
                    &mut c.out, keyboard, serial, depressed, latched, locked, m.group,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{FakeImports, TestClient};
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_registry};
    use crate::wayland::wire::Arg;

    const KEY_A: u32 = 30;
    const SHIFT: Modifiers = Modifiers {
        depressed: 1,
        latched: 0,
        locked: 0,
        group: 0,
    };

    /// A client with registry 2, compositor 3, seat 4, keyboard 5 and the
    /// surface 6, a toplevel of 100 by 50 at the output's corner.
    fn with_window(server: &mut Server) -> (TestClient, SurfaceId) {
        server.set_screen((1280, 800));
        let keymap = b"xkb_keymap {};\0".to_vec();
        server.set_keymap(&mut FakeImports::default(), keymap, Modifiers::default());
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &wl_seat::INTERFACE, 4);
        client.send(server, 4, 1, vec![Arg::NewId(NewId(5))]);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(6))]);
        let (&id, surface) = server.surfaces.iter_mut().last().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        server.move_window(id, 0, 0);
        (client, id)
    }

    fn keyboard_events(server: &mut Server, client: &TestClient) -> Vec<(u32, u16, Vec<Arg>)> {
        client
            .events(server, &|id| match id {
                4 => &wl_seat::EVENTS,
                5 => &wl_keyboard::EVENTS,
                _ => &wl_registry::EVENTS,
            })
            .into_iter()
            .filter(|e| e.0 == 5)
            .collect()
    }

    #[test]
    fn gives_each_keyboard_the_keymap() {
        let mut server = Server::new();
        let (client, _) = with_window(&mut server);
        let events = keyboard_events(&mut server, &client);
        assert_eq!(events.len(), 2);
        let (_, opcode, args) = &events[0];
        assert_eq!(*opcode, 0);
        assert_eq!(args[0], Arg::Uint(wl_keyboard::keymap_format::XKB_V1));
        assert!(matches!(args[1], Arg::Fd(_)));
        assert_eq!(args[2], Arg::Uint(15));
        // Then the key repeat, the default of MIKSPEC-0066.R29.
        assert_eq!(events[1], (5, 5, vec![Arg::Int(25), Arg::Int(600)]));
    }

    #[test]
    fn sends_another_keymap_to_every_keyboard() {
        let mut server = Server::new();
        let (client, window) = with_window(&mut server);
        server.focus_keyboard(Some(window));
        keyboard_events(&mut server, &client);
        let mut imports = FakeImports::default();
        let same = b"xkb_keymap {};\0".to_vec();
        server.set_keymap(&mut imports, same, Modifiers::default());
        assert!(keyboard_events(&mut server, &client).is_empty());
        let other = b"xkb_keymap { de };\0".to_vec();
        let size = other.len() as u32;
        server.set_keymap(&mut imports, other, SHIFT);
        let events = keyboard_events(&mut server, &client);
        assert_eq!(events.iter().map(|e| e.1).collect::<Vec<_>>(), [0, 4]);
        assert_eq!(events[0].2[2], Arg::Uint(size));
        assert_eq!(events[1].2[1], Arg::Uint(1));
    }

    #[test]
    fn sends_the_key_repeat_again_when_it_changes() {
        let mut server = Server::new();
        let (client, _) = with_window(&mut server);
        keyboard_events(&mut server, &client);
        assert!(!server.set_repeat(25, 600));
        assert!(server.set_repeat(0, 600));
        let events = keyboard_events(&mut server, &client);
        assert_eq!(events, [(5, 5, vec![Arg::Int(0), Arg::Int(600)])]);
    }

    #[test]
    fn has_no_keyboard_without_a_keymap() {
        let mut server = Server::new();
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &wl_seat::INTERFACE, 4);
        client.send(&mut server, 4, 1, vec![Arg::NewId(NewId(5))]);
        let missing = wl_seat::error::MISSING_CAPABILITY;
        assert_eq!(client.error(&mut server), Some(missing));
    }

    #[test]
    fn sends_keys_to_the_window_clicked() {
        let mut server = Server::new();
        let (client, window) = with_window(&mut server);
        keyboard_events(&mut server, &client);
        // No window has the focus yet.
        server.keyboard_key(KEY_A, true, 1, Modifiers::default());
        server.keyboard_key(KEY_A, false, 2, Modifiers::default());
        assert!(keyboard_events(&mut server, &client).is_empty());

        server.pointer_motion(10.0, 10.0, 3);
        server.pointer_button(0x110, true, 4);
        server.pointer_button(0x110, false, 5);
        assert_eq!(server.keyboard_focus(), Some(window));
        server.keyboard_key(42, true, 6, SHIFT);
        server.keyboard_key(KEY_A, true, 7, SHIFT);
        let events = keyboard_events(&mut server, &client);
        let opcodes: Vec<u16> = events.iter().map(|e| e.1).collect();
        // enter, modifiers, then Shift with its modifiers, then A.
        assert_eq!(opcodes, [1, 4, 3, 4, 3]);
        assert_eq!(events[0].2[2], Arg::Array(Vec::new()));
        assert_eq!(events[2].2[2], Arg::Uint(42));
        assert_eq!(events[3].2[1], Arg::Uint(1));
        assert_eq!(events[4].2[2], Arg::Uint(KEY_A));
        assert_eq!(events[4].2[3], Arg::Uint(wl_keyboard::key_state::PRESSED));
    }

    #[test]
    fn enters_with_the_keys_held_and_moves_on_when_the_window_goes() {
        let mut server = Server::new();
        let (client, window) = with_window(&mut server);
        server.keyboard_key(KEY_A, true, 1, SHIFT);
        server.focus_keyboard(Some(window));
        let events = keyboard_events(&mut server, &client);
        let enter = events.iter().find(|e| e.1 == 1).unwrap();
        assert_eq!(enter.2[2], Arg::Array(KEY_A.to_le_bytes().to_vec()));
        let modifiers = events.iter().find(|e| e.1 == 4).unwrap();
        assert_eq!(modifiers.2[1], Arg::Uint(1));

        server.surfaces.get_mut(&window).unwrap().current.buffer = None;
        server.keep_keyboard_focus();
        assert_eq!(server.keyboard_focus(), None);
        let events = keyboard_events(&mut server, &client);
        assert_eq!(events.iter().map(|e| e.1).collect::<Vec<_>>(), [2]);
    }
}
