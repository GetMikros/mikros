//! The window switcher and the Super key (MIKSPEC-0074.R13): the compositor
//! keeps the windows in order of last focus; Alt and Tab, or with Shift
//! backwards, start the switcher, which keeps the keyboard focus itself
//! while Alt is held, moves its selection with Tab and Shift and Tab, and
//! when Alt is let go focuses, raises and restores the selected window;
//! Escape closes it without a change. The `switcher` component shows the
//! list and the selection (MIKSPEC-0076.R12), and a Super key pressed and
//! released alone asks the `start-menu` component to toggle its menu.

use super::{Resource, Server, SurfaceId};
use crate::wayland::protocols::mikros_shell_v1;
use crate::wayland::wire::{ObjectId, Outgoing};

/// The switcher that is open: the windows in order, the one selected and
/// the window that had the keyboard before.
#[derive(Debug)]
struct Open {
    items: Vec<SurfaceId>,
    selected: usize,
    before: Option<SurfaceId>,
}

/// What the switcher keeps: the windows in order of last focus, the
/// switcher that is open, and whether a Super key was pressed with no other
/// key since.
#[derive(Debug, Default)]
pub(super) struct Switching {
    recent: Vec<SurfaceId>,
    open: Option<Open>,
    super_alone: bool,
}

impl Switching {
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The window that had the keyboard before the open switcher took it,
    /// which a new window is placed by as if it still had it.
    pub(super) fn before(&self) -> Option<SurfaceId> {
        self.open.as_ref().and_then(|o| o.before)
    }
}

impl Server {
    /// Notes that the window `surface` had the keyboard focus last.
    pub(super) fn remember_focus(&mut self, surface: SurfaceId) {
        self.switching.recent.retain(|&s| s != surface);
        self.switching.recent.insert(0, surface);
    }

    /// The windows in order of last focus, then the others from the top of
    /// the stack down.
    fn switch_order(&self) -> Vec<SurfaceId> {
        let windows: Vec<SurfaceId> = self.windows().iter().map(|w| w.surface).collect();
        let mut order: Vec<SurfaceId> = self
            .switching
            .recent
            .iter()
            .copied()
            .filter(|s| windows.contains(s))
            .collect();
        let rest: Vec<SurfaceId> = windows
            .iter()
            .rev()
            .copied()
            .filter(|s| !order.contains(s))
            .collect();
        order.extend(rest);
        order
    }

    /// Sends the events made by `send` to the components of `role`, to
    /// each object of their shell global, with the client's `wl_output`.
    fn tell_role(&mut self, role: &str, send: &dyn Fn(&mut Outgoing, ObjectId, Option<ObjectId>)) {
        let clients: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.has_role(role))
            .map(|(&id, _)| id)
            .collect();
        for client in clients {
            let shells = self.objects_of(client, Resource::Shell);
            let output = self
                .objects_of(client, Resource::Output)
                .first()
                .map(|&(id, _)| id);
            if let Some(c) = self.clients.get_mut(&client) {
                for (shell, _) in shells {
                    send(&mut c.out, shell, output);
                }
            }
        }
    }

    /// The app ID and title that the switcher shows for a window.
    fn switcher_item(&self, surface: SurfaceId) -> (String, String) {
        let Some(s) = self.surfaces.get(&surface) else {
            return Default::default();
        };
        let app_id = self
            .clients
            .get(&s.client)
            .and_then(|c| c.app_id.clone())
            .unwrap_or_default();
        let title = s.xdg.as_ref().map(|x| x.title.clone()).unwrap_or_default();
        (app_id, title)
    }

    /// Starts the switcher, or moves its selection when it is open, to the
    /// next window or, `backwards`, the previous.
    pub(super) fn switch_windows(&mut self, backwards: bool) {
        if let Some(open) = self.switching.open.as_mut() {
            let n = open.items.len();
            open.selected = if backwards {
                (open.selected + n - 1) % n
            } else {
                (open.selected + 1) % n
            };
            let selected = open.selected as u32;
            self.tell_role("switcher", &|out, shell, _| {
                mikros_shell_v1::send_switcher_select(out, shell, selected);
            });
            return;
        }
        let items = self.switch_order();
        if items.is_empty() {
            return;
        }
        let selected = match (backwards, items.len()) {
            (_, 1) => 0,
            (true, n) => n - 1,
            (false, _) => 1,
        };
        let before = self.keyboard_focus();
        // The compositor keeps the keyboard while Alt is held.
        self.focus_keyboard(None);
        let shown: Vec<(String, String)> = items.iter().map(|&s| self.switcher_item(s)).collect();
        self.switching.open = Some(Open {
            items,
            selected,
            before,
        });
        self.tell_role("switcher", &|out, shell, _| {
            mikros_shell_v1::send_switcher_show(out, shell);
            for (app_id, title) in &shown {
                mikros_shell_v1::send_switcher_item(out, shell, app_id, title);
            }
            mikros_shell_v1::send_switcher_select(out, shell, selected as u32);
        });
    }

    /// Closes the switcher: the selected window gets the keyboard, is raised
    /// and restored when `choose`, and otherwise the window that had it
    /// before gets it back.
    pub(super) fn end_switcher(&mut self, choose: bool) {
        let Some(open) = self.switching.open.take() else {
            return;
        };
        self.tell_role("switcher", &|out, shell, _| {
            mikros_shell_v1::send_switcher_hide(out, shell);
        });
        let target = if choose {
            open.items.get(open.selected).copied()
        } else {
            open.before
        };
        let Some(surface) = target else {
            return;
        };
        if choose {
            if let Some(xdg) = self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut()) {
                xdg.minimized = false;
            }
            self.raise(surface);
        }
        self.focus_keyboard(Some(surface));
    }

    /// `switcher_choose` of the `switcher` component: the window at `index`
    /// is chosen as if Alt was let go.
    pub(super) fn choose_in_switcher(&mut self, index: u32) {
        if let Some(open) = self.switching.open.as_mut()
            && (index as usize) < open.items.len()
        {
            open.selected = index as usize;
            self.end_switcher(true);
        }
    }

    /// A Super key was pressed or let go: let go with no other key since,
    /// it asks the `start-menu` component to toggle its menu.
    pub(super) fn super_pressed(&mut self, pressed: bool) {
        if pressed {
            self.switching.super_alone = true;
        } else if std::mem::take(&mut self.switching.super_alone) {
            self.tell_role("start-menu", &|out, shell, output| {
                if let Some(output) = output {
                    mikros_shell_v1::send_toggle_start_menu(out, shell, output);
                }
            });
        }
    }

    /// Another key was pressed: a Super key before it was not alone.
    pub(super) fn other_key_pressed(&mut self) {
        self.switching.super_alone = false;
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::super::windows::tests::toplevel;
    use super::super::xdg::XdgState;
    use super::*;
    use crate::wayland::protocols::{wl_display, wl_output, wl_registry};
    use crate::wayland::wire::{Arg, NewId};

    const SHELL: ObjectId = 5;

    /// A component of the roles switcher and start-menu with registry 2, the
    /// output 4 and the shell global 5, and the windows 1, 2 and 3.
    fn setup() -> (Server, TestClient) {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        server.next_surface = 100;
        let client = TestClient {
            id: server
                .connect_component(vec!["switcher".into(), "start-menu".into()], "test".into()),
            imports: Default::default(),
        };
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &wl_output::INTERFACE, 4);
        client.bind(&mut server, 2, &mikros_shell_v1::INTERFACE, SHELL);
        for (id, buffer) in [(1, 10), (2, 11), (3, 12)] {
            toplevel(&mut server, id, buffer, (400, 300));
            let mut xdg = XdgState::windowed(None, false);
            xdg.title = format!("window {id}");
            server.surfaces.get_mut(&id).unwrap().xdg = Some(xdg);
        }
        (server, client)
    }

    fn events(server: &mut Server, client: &TestClient) -> Vec<(u16, Vec<Arg>)> {
        client
            .events(server, &|id| match id {
                1 => &wl_display::EVENTS,
                2 => &wl_registry::EVENTS,
                4 => &wl_output::EVENTS,
                _ => &mikros_shell_v1::EVENTS,
            })
            .into_iter()
            .filter(|e| e.0 == SHELL)
            .map(|e| (e.1, e.2))
            .collect()
    }

    fn press(server: &mut Server, keys: &[u32]) {
        for &key in keys {
            server.keyboard_key(key, true, 0, Default::default());
        }
    }

    fn release(server: &mut Server, keys: &[u32]) {
        for &key in keys.iter().rev() {
            server.keyboard_key(key, false, 0, Default::default());
        }
    }

    const ALT: u32 = 56;
    const TAB: u32 = 15;
    const SHIFT: u32 = 42;
    const ESC: u32 = 1;
    const SUPER: u32 = 125;

    #[test]
    fn windows_are_in_order_of_last_focus_then_from_the_top_of_the_stack() {
        let (mut server, _) = setup();
        server.focus_keyboard(Some(1));
        server.focus_keyboard(Some(3));
        server.focus_keyboard(Some(1));
        assert_eq!(server.switch_order(), [1, 3, 2]);
    }

    #[test]
    fn alt_and_tab_show_the_switcher_move_the_selection_and_choose_on_alt_up() {
        let (mut server, client) = setup();
        server.focus_keyboard(Some(2));
        server.focus_keyboard(Some(1));
        events(&mut server, &client);
        press(&mut server, &[ALT, TAB]);
        release(&mut server, &[TAB]);
        let sent = events(&mut server, &client);
        let kinds: Vec<u16> = sent.iter().map(|e| e.0).collect();
        // show, three items, the selection of the next window.
        assert_eq!(kinds, [1, 2, 2, 2, 3]);
        assert_eq!(sent[1].1[1], Arg::String(Some("window 1".into())));
        assert_eq!(sent[4].1, [Arg::Uint(1)]);
        // The compositor holds the keyboard while Alt is held.
        assert_eq!(server.keyboard_focus(), None);
        press(&mut server, &[TAB]);
        release(&mut server, &[TAB]);
        assert_eq!(events(&mut server, &client)[0].1, [Arg::Uint(2)]);
        // Shift and Tab go back.
        press(&mut server, &[SHIFT, TAB]);
        release(&mut server, &[TAB, SHIFT]);
        assert_eq!(events(&mut server, &client)[0].1, [Arg::Uint(1)]);
        // Alt let go: window 2, the selection, gets the keyboard, on top.
        release(&mut server, &[ALT]);
        assert_eq!(events(&mut server, &client)[0].0, 4);
        assert_eq!(server.keyboard_focus(), Some(2));
        assert_eq!(server.windows().last().unwrap().surface, 2);
    }

    #[test]
    fn shift_tab_starts_at_the_last_window_and_escape_changes_nothing() {
        let (mut server, client) = setup();
        server.focus_keyboard(Some(2));
        events(&mut server, &client);
        press(&mut server, &[ALT, SHIFT, TAB]);
        let sent = events(&mut server, &client);
        assert_eq!(sent.last().unwrap().1, [Arg::Uint(2)]);
        release(&mut server, &[TAB, SHIFT]);
        press(&mut server, &[ESC]);
        release(&mut server, &[ESC, ALT]);
        let sent = events(&mut server, &client);
        // Hidden once, without a second hide when Alt is let go.
        assert_eq!(sent.iter().filter(|e| e.0 == 4).count(), 1);
        assert_eq!(server.keyboard_focus(), Some(2));
    }

    #[test]
    fn a_minimized_window_is_restored_when_it_is_chosen() {
        let (mut server, client) = setup();
        server.focus_keyboard(Some(1));
        // The order is 1, then 3 and 2 from the top of the stack down.
        server
            .surfaces
            .get_mut(&3)
            .unwrap()
            .xdg
            .as_mut()
            .unwrap()
            .minimized = true;
        events(&mut server, &client);
        press(&mut server, &[ALT, TAB]);
        release(&mut server, &[TAB, ALT]);
        assert_eq!(server.keyboard_focus(), Some(3));
        assert!(!server.surfaces[&3].xdg.as_ref().unwrap().minimized);
    }

    #[test]
    fn the_switcher_component_can_choose_a_window() {
        let (mut server, client) = setup();
        server.focus_keyboard(Some(1));
        press(&mut server, &[ALT, TAB]);
        release(&mut server, &[TAB]);
        events(&mut server, &client);
        let third = server.switch_order()[2];
        client.send(&mut server, SHELL, 12, vec![Arg::Uint(2)]);
        assert_eq!(client.error(&mut server), None);
        assert_eq!(server.keyboard_focus(), Some(third));
    }

    #[test]
    fn a_super_key_alone_toggles_the_start_menu_and_with_another_key_does_not() {
        let (mut server, client) = setup();
        server.focus_keyboard(Some(1));
        events(&mut server, &client);
        press(&mut server, &[SUPER]);
        release(&mut server, &[SUPER]);
        let sent = events(&mut server, &client);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (sent[0].0, &sent[0].1[..]),
            (0, &[Arg::Object(Some(4))][..])
        );
        press(&mut server, &[SUPER, 103]);
        release(&mut server, &[103, SUPER]);
        assert!(events(&mut server, &client).is_empty());
    }

    #[test]
    fn a_super_drag_does_not_toggle_the_start_menu() {
        let (mut server, client) = setup();
        server.pointer_motion(500.0, 300.0, 1);
        events(&mut server, &client);
        press(&mut server, &[SUPER]);
        server.pointer_button(0x110, true, 2);
        server.pointer_motion(600.0, 400.0, 3);
        server.pointer_button(0x110, false, 4);
        release(&mut server, &[SUPER]);
        assert!(events(&mut server, &client).is_empty());
    }

    #[test]
    fn a_window_that_maps_while_the_switcher_is_open_does_not_take_the_keyboard() {
        let (mut server, client) = setup();
        // The windows that were mapped first have been settled.
        server.focus_new_windows();
        server.focus_keyboard(Some(1));
        press(&mut server, &[ALT, TAB]);
        release(&mut server, &[TAB]);
        events(&mut server, &client);
        toplevel(&mut server, 4, 13, (400, 300));
        server.focus_new_windows();
        assert_eq!(server.keyboard_focus(), None);
        release(&mut server, &[ALT]);
        server.focus_new_windows();
        // The selection has the keyboard, and the new window waits below.
        assert_eq!(server.keyboard_focus(), Some(3));
        assert!(
            server
                .windows()
                .iter()
                .find(|w| w.surface == 4)
                .unwrap()
                .attention
        );
    }

    #[test]
    fn minimizing_the_focused_window_moves_the_keyboard_to_the_next_at_once() {
        const DOWN: u32 = 108;
        let (mut server, _client) = setup();
        server.focus_new_windows();
        server.focus_keyboard(Some(1));
        let order: Vec<SurfaceId> = server.windows().iter().map(|w| w.surface).collect();
        let top = *order.last().unwrap();
        server.focus_keyboard(Some(top));
        // Super and Down minimize the focused window, and the keyboard goes
        // to the highest window that is left, before any pointer event.
        press(&mut server, &[SUPER, DOWN]);
        release(&mut server, &[SUPER, DOWN]);
        let next = server.keyboard_focus();
        assert!(next.is_some() && next != Some(top));
        press(&mut server, &[SUPER, DOWN]);
        release(&mut server, &[SUPER, DOWN]);
        let last = server.keyboard_focus();
        assert!(last.is_some() && last != next && last != Some(top));
    }
}
