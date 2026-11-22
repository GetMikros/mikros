//! `xdg_activation_v1` and its tokens (MIKSPEC-0074.R7). A token that
//! the compositor issues only to a shell component, or to a client that
//! gives the serial of a key or button press that it received while it
//! had the keyboard focus, at most ten seconds earlier, expires after ten
//! seconds or at its first use, which activates the window it names:
//! raises it and gives it the keyboard focus. Any other client gets a
//! token that activates nothing. A window that is activated before it is
//! mapped is remembered until it maps.

use super::{ClientId, ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{wl_seat, xdg_activation_token_v1, xdg_activation_v1};
use crate::wayland::wire::{Arg, ObjectId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const VERSION: u32 = 1;
/// How long a token and an input serial may be used, in milliseconds.
const LIFETIME: u64 = 10_000;
/// The most press serials that are kept.
const SERIALS: usize = 64;
/// The most tokens that are valid at once.
const TOKENS: usize = 256;

/// A token that was issued: its value and when.
#[derive(Debug)]
struct Issued {
    value: String,
    at: u64,
}

/// A token object's serial, and whether it was committed.
#[derive(Debug, Default)]
struct Making {
    serial: Option<u32>,
    committed: bool,
}

/// The serial of a press that a client received.
#[derive(Debug)]
struct Press {
    serial: u32,
    client: ClientId,
    at: u64,
    /// Whether the client had the keyboard focus.
    focused: bool,
}

/// What the activation of windows keeps.
#[derive(Debug, Default)]
pub struct Activation {
    /// The time of the monotonic clock in milliseconds, which the program
    /// sets.
    clock: u64,
    tokens: Vec<Issued>,
    presses: VecDeque<Press>,
    /// The serials that the tokens being made name, by client and object.
    making: BTreeMap<(ClientId, ObjectId), Making>,
    /// The surfaces that were activated and are not yet windows.
    pub(super) waiting: BTreeSet<SurfaceId>,
    random: u64,
}

impl Activation {
    /// A value for a token, from a generator that the program seeds.
    fn value(&mut self) -> String {
        // splitmix64
        self.random = self.random.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.random;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        format!("{:016x}", z ^ (z >> 31))
    }

    fn expire(&mut self) {
        let clock = self.clock;
        self.tokens
            .retain(|t| clock.saturating_sub(t.at) < LIFETIME);
        self.presses
            .retain(|p| clock.saturating_sub(p.at) < LIFETIME);
    }
}

impl Server {
    /// Sets the time of the monotonic clock, in milliseconds, which the
    /// tokens' lifetimes are measured by.
    pub fn set_clock(&mut self, milliseconds: u64) {
        self.activation.clock = milliseconds;
    }

    /// Seeds the values of tokens, which other clients must not guess.
    pub fn seed_tokens(&mut self, seed: u64) {
        self.activation.random = seed;
    }

    /// Records that `client` got a press with `serial`, which it may name
    /// for a token if it had the keyboard focus.
    pub(super) fn record_press(&mut self, client: ClientId, serial: u32) {
        let focused = self.keyboard_client() == Some(client);
        let a = &mut self.activation;
        a.presses.push_back(Press {
            serial,
            client,
            at: a.clock,
            focused,
        });
        while a.presses.len() > SERIALS {
            a.presses.pop_front();
        }
    }

    pub(super) fn activation_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::Activation => {
                match xdg_activation_v1::Request::decode(request.opcode, args).map_err(invalid)? {
                    xdg_activation_v1::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    xdg_activation_v1::Request::GetActivationToken { id } => {
                        let interface = &xdg_activation_token_v1::INTERFACE;
                        let c = self.client_mut(request);
                        c.create(id, interface, request.version, Resource::ActivationToken)?;
                        self.activation
                            .making
                            .insert((request.client, id.0), Making::default());
                    }
                    xdg_activation_v1::Request::Activate { token, surface } => {
                        let c = self.client_mut(request);
                        let Resource::Surface(id) = c.resource(
                            request.object,
                            surface,
                            &crate::wayland::protocols::wl_surface::INTERFACE,
                        )?
                        else {
                            unreachable!("a wl_surface object is a surface");
                        };
                        self.activate_with(&token, id);
                    }
                }
                Ok(())
            }
            Resource::ActivationToken => {
                let key = (request.client, request.object);
                match xdg_activation_token_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    xdg_activation_token_v1::Request::SetSerial { serial, seat } => {
                        self.client_mut(request).resource(
                            request.object,
                            seat,
                            &wl_seat::INTERFACE,
                        )?;
                        if let Some(made) = self.activation.making.get_mut(&key) {
                            made.serial = Some(serial);
                        }
                    }
                    xdg_activation_token_v1::Request::SetAppId { .. }
                    | xdg_activation_token_v1::Request::SetSurface { .. } => {}
                    xdg_activation_token_v1::Request::Commit => {
                        let serial = match self.activation.making.get_mut(&key) {
                            Some(made) if !made.committed => {
                                made.committed = true;
                                made.serial
                            }
                            _ => {
                                return Err(ProtocolError::new(
                                    request.object,
                                    xdg_activation_token_v1::error::ALREADY_USED,
                                    "a token that was committed already",
                                ));
                            }
                        };
                        let value = self.issue_token(request.client, serial);
                        let c = self.client_mut(request);
                        xdg_activation_token_v1::send_done(&mut c.out, request.object, &value);
                    }
                    xdg_activation_token_v1::Request::Destroy => {
                        self.activation.making.remove(&key);
                        self.client_mut(request).destroy(request.object);
                    }
                }
                Ok(())
            }
            _ => unreachable!("activation_request handles activation"),
        }
    }

    /// The client whose window has the keyboard focus.
    fn keyboard_client(&self) -> Option<ClientId> {
        self.surfaces.get(&self.keyboard_focus()?).map(|s| s.client)
    }

    /// A value for a token of `client`, which is valid when the client is a
    /// shell component or `serial` is that of a press that it received
    /// while it had the keyboard focus within the lifetime of a token.
    fn issue_token(&mut self, client: ClientId, serial: Option<u32>) -> String {
        self.activation.expire();
        let component = self.clients.get(&client).is_some_and(|c| c.roles.is_some());
        let pressed = serial.is_some_and(|serial| {
            self.activation
                .presses
                .iter()
                .any(|p| p.client == client && p.serial == serial && p.focused)
        });
        let value = self.activation.value();
        if component || pressed {
            let at = self.activation.clock;
            let tokens = &mut self.activation.tokens;
            tokens.push(Issued {
                value: value.clone(),
                at,
            });
            if tokens.len() > TOKENS {
                tokens.remove(0);
            }
        }
        value
    }

    /// Uses `token`, if it is valid, to activate the surface `surface`.
    fn activate_with(&mut self, token: &str, surface: SurfaceId) {
        self.activation.expire();
        let Some(at) = self.activation.tokens.iter().position(|t| t.value == token) else {
            return;
        };
        self.activation.tokens.remove(at);
        self.activate(surface);
    }

    /// Raises the window of `surface` and gives it the keyboard focus, or,
    /// when it is not a window yet, remembers to when it becomes one.
    pub fn activate(&mut self, surface: SurfaceId) {
        if self.windows().iter().any(|w| w.surface == surface) {
            self.raise(surface);
            self.focus_keyboard(Some(surface));
        } else if self.surfaces.contains_key(&surface) {
            self.activation.waiting.insert(surface);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::super::windows::tests::toplevel;
    use super::super::{Role, Server};
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_display, wl_registry};
    use crate::wayland::wire::NewId;

    /// A server with a time and the windows 1 and 2.
    fn server() -> Server {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        server.set_clock(1_000);
        server.seed_tokens(7);
        server.next_surface = 100;
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (400, 300));
        server
    }

    /// A client, a shell component when `component`, with registry 2,
    /// compositor 3, the activation global 4, the token 5, the seat 8 and a
    /// window, the surface 6, below window 1. Returns it and its window.
    fn connect(server: &mut Server, component: bool) -> (TestClient, SurfaceId) {
        let id = if component {
            server.connect_component(vec!["taskbar".into()], "test.component".into())
        } else {
            server.connect()
        };
        let client = TestClient {
            id,
            imports: Default::default(),
        };
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(server, 2, &xdg_activation_v1::INTERFACE, 4);
        client.bind(server, 2, &wl_seat::INTERFACE, 8);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(6))]);
        server.buffer_sizes.insert(99, (100, 50));
        let (&own, surface) = server.surfaces.iter_mut().last().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.windows();
        server.raise(1);
        assert_eq!(order(server), [2, own, 1]);
        (client, own)
    }

    fn order(server: &Server) -> Vec<SurfaceId> {
        server.windows().iter().map(|w| w.surface).collect()
    }

    /// A new token object of `client`, and its ID.
    fn new_token(server: &mut Server, client: &TestClient) -> ObjectId {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(20);
        let object = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        client.send(server, 4, 1, vec![Arg::NewId(NewId(object))]);
        object
    }

    /// The value of a token that `client` commits, with `serial`.
    fn token(server: &mut Server, client: &TestClient, serial: Option<u32>) -> String {
        let object = new_token(server, client);
        if let Some(serial) = serial {
            let arguments = vec![Arg::Uint(serial), Arg::Object(Some(8))];
            client.send(server, object, 0, arguments);
        }
        client.send(server, object, 3, vec![]);
        let events = client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            8 => &wl_seat::EVENTS,
            _ => &xdg_activation_token_v1::EVENTS,
        });
        let done = events.iter().find(|e| (e.0, e.1) == (object, 0)).unwrap();
        let Arg::String(Some(value)) = &done.2[0] else {
            panic!("a token value: {events:?}");
        };
        value.clone()
    }

    fn activate(server: &mut Server, client: &TestClient, token: &str) {
        let arguments = vec![Arg::String(Some(token.to_owned())), Arg::Object(Some(6))];
        client.send(server, 4, 2, arguments);
    }

    #[test]
    fn a_component_gets_a_token_that_activates_its_window_once() {
        let mut server = server();
        let (client, own) = connect(&mut server, true);
        let value = token(&mut server, &client, None);
        activate(&mut server, &client, &value);
        assert_eq!(server.keyboard_focus(), Some(own));
        assert_eq!(order(&server), [2, 1, own]);
        server.raise(1);
        activate(&mut server, &client, &value);
        assert_eq!(order(&server), [2, own, 1]);
    }

    #[test]
    fn a_token_expires_after_ten_seconds() {
        let mut server = server();
        let (client, own) = connect(&mut server, true);
        let value = token(&mut server, &client, None);
        server.set_clock(1_000 + LIFETIME - 1);
        let other = token(&mut server, &client, None);
        server.set_clock(1_000 + LIFETIME);
        activate(&mut server, &client, &value);
        assert_ne!(server.keyboard_focus(), Some(own));
        activate(&mut server, &client, &other);
        assert_eq!(server.keyboard_focus(), Some(own));
    }

    #[test]
    fn a_client_gets_a_valid_token_only_for_a_press_it_got_with_the_focus() {
        let mut server = server();
        let (client, own) = connect(&mut server, false);
        // No serial, or a serial that is not of a press: nothing happens.
        let value = token(&mut server, &client, None);
        activate(&mut server, &client, &value);
        assert_ne!(server.keyboard_focus(), Some(own));
        // A press that it got without the keyboard focus.
        server.record_press(client.id, 41);
        let value = token(&mut server, &client, Some(41));
        activate(&mut server, &client, &value);
        assert_ne!(server.keyboard_focus(), Some(own));
        // A press that it got with it, whatever has the focus later.
        server.focus_keyboard(Some(own));
        server.record_press(client.id, 42);
        server.focus_keyboard(Some(1));
        let value = token(&mut server, &client, Some(42));
        activate(&mut server, &client, &value);
        assert_eq!(server.keyboard_focus(), Some(own));
    }

    #[test]
    fn a_token_that_is_committed_twice_is_an_error() {
        let mut server = server();
        let (client, _) = connect(&mut server, true);
        let object = new_token(&mut server, &client);
        client.send(&mut server, object, 3, vec![]);
        client.send(&mut server, object, 3, vec![]);
        assert_eq!(
            client.error(&mut server),
            Some(xdg_activation_token_v1::error::ALREADY_USED)
        );
    }

    #[test]
    fn a_window_activated_before_it_is_mapped_is_remembered() {
        let mut server = server();
        let (client, _) = connect(&mut server, true);
        let value = token(&mut server, &client, None);
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(10))]);
        let later = *server.surfaces.keys().last().unwrap();
        client.send(
            &mut server,
            4,
            2,
            vec![Arg::String(Some(value)), Arg::Object(Some(10))],
        );
        assert!(server.activation.waiting.contains(&later));
    }
}
