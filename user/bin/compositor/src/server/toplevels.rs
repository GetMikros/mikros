//! The window list of the shell (MIKSPEC-0076.R5): a
//! `mikros_toplevel_list_v1` of a component of the role `menubar`,
//! `taskbar` or `capture-picker` has a `mikros_toplevel_handle_v1` for every
//! window, which tells the window's title, app ID, state, output and parent
//! and when it closed, and which activates, minimizes and closes the window.
//! The lists are brought up to date by `update_toplevel_lists`, which the
//! program calls for each frame, so the events of one change come in one
//! piece, ending with `done`.

use super::{ClientId, ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{
    mikros_shell_v1, mikros_toplevel_handle_v1, mikros_toplevel_list_v1, wl_seat, wl_surface,
    xdg_toplevel,
};
use crate::wayland::wire::{Arg, NewId, ObjectId};
use std::collections::BTreeMap;

/// The values of the `state` array.
const FOCUSED: u32 = 1;
const MINIMIZED: u32 = 2;
const MAXIMIZED: u32 = 3;
const FULLSCREEN: u32 = 4;
const ATTENTION: u32 = 5;

/// What a handle tells about its window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Info {
    title: String,
    app_id: String,
    state: Vec<u32>,
    parent: Option<SurfaceId>,
    output: Option<ObjectId>,
}

/// The handle of a window in a list, and what it told last.
#[derive(Debug)]
struct Handle {
    object: ObjectId,
    told: Option<Info>,
}

/// A window list of a client.
#[derive(Debug, Default)]
struct List {
    handles: BTreeMap<SurfaceId, Handle>,
    stopped: bool,
}

/// The window lists of every client, by the client and the list's object.
#[derive(Debug, Default)]
pub(super) struct Lists {
    lists: BTreeMap<(ClientId, ObjectId), List>,
}

impl Server {
    /// Makes the window list `id` of a component of the shell global's
    /// request `request`.
    pub(super) fn get_toplevel_list(
        &mut self,
        request: Request,
        id: NewId,
    ) -> Result<(), ProtocolError> {
        let c = self.client_mut(request);
        if !["menubar", "taskbar", "capture-picker"]
            .iter()
            .any(|role| c.has_role(role))
        {
            return Err(ProtocolError::new(
                request.object,
                mikros_shell_v1::error::FORBIDDEN,
                "get_toplevel_list needs the role menubar, taskbar or capture-picker",
            ));
        }
        let interface = &mikros_toplevel_list_v1::INTERFACE;
        c.create(id, interface, request.version, Resource::ToplevelList)?;
        self.toplevels
            .lists
            .insert((request.client, id.0), List::default());
        Ok(())
    }

    pub(super) fn toplevel_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::ToplevelList => {
                let key = (request.client, request.object);
                match mikros_toplevel_list_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    mikros_toplevel_list_v1::Request::Stop => {
                        if let Some(list) = self.toplevels.lists.get_mut(&key)
                            && !list.stopped
                        {
                            list.stopped = true;
                            let c = self.client_mut(request);
                            mikros_toplevel_list_v1::send_finished(&mut c.out, request.object);
                        }
                    }
                    mikros_toplevel_list_v1::Request::Destroy => {
                        self.toplevels.lists.remove(&key);
                        self.client_mut(request).destroy(request.object);
                    }
                }
                Ok(())
            }
            Resource::ToplevelHandle(surface) => {
                match mikros_toplevel_handle_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    mikros_toplevel_handle_v1::Request::Activate { seat } => {
                        self.client_mut(request).resource(
                            request.object,
                            seat,
                            &wl_seat::INTERFACE,
                        )?;
                        if let Some(xdg) =
                            self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut())
                        {
                            xdg.minimized = false;
                        }
                        self.activate(surface);
                    }
                    mikros_toplevel_handle_v1::Request::Minimize => {
                        if let Some(xdg) =
                            self.surfaces.get_mut(&surface).and_then(|s| s.xdg.as_mut())
                        {
                            xdg.minimized = true;
                        }
                    }
                    mikros_toplevel_handle_v1::Request::Close => self.close_window(surface),
                    mikros_toplevel_handle_v1::Request::SetMinimizeTarget { surface, .. } => {
                        self.client_mut(request).resource(
                            request.object,
                            surface,
                            &wl_surface::INTERFACE,
                        )?;
                    }
                    mikros_toplevel_handle_v1::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                }
                Ok(())
            }
            _ => unreachable!("toplevel_request handles window lists"),
        }
    }

    /// Forgets the window lists and the unfinished activation tokens of
    /// `client`, which disconnected.
    pub(super) fn forget_client_lists(&mut self, client: ClientId) {
        self.toplevels.lists.retain(|(c, _), _| *c != client);
        self.activation.forget_client(client);
    }

    /// Asks the client of the window `surface` to close it, with
    /// `xdg_toplevel.close`, and does nothing else to it.
    pub(super) fn close_window(&mut self, surface: SurfaceId) {
        let Some(s) = self.surfaces.get(&surface) else {
            return;
        };
        let (client, toplevel) = (s.client, s.xdg.as_ref().and_then(|x| x.toplevel));
        let (Some(toplevel), Some(c)) = (toplevel, self.clients.get_mut(&client)) else {
            return;
        };
        xdg_toplevel::send_close(&mut c.out, toplevel);
    }

    /// What the window `surface` tells in the lists of the client that
    /// has `output` as its `wl_output`.
    fn window_info(&self, surface: SurfaceId, output: Option<ObjectId>) -> Info {
        let Some(s) = self.surfaces.get(&surface) else {
            return Info::default();
        };
        let xdg = s.xdg.as_ref();
        let mut state = Vec::new();
        let window = self.windows().into_iter().find(|w| w.surface == surface);
        if self.keyboard_focus() == Some(surface) {
            state.push(FOCUSED);
        }
        if xdg.is_some_and(|x| x.minimized) {
            state.push(MINIMIZED);
        }
        if xdg.is_some_and(|x| x.maximized) {
            state.push(MAXIMIZED);
        }
        if xdg.is_some_and(|x| x.fullscreen) {
            state.push(FULLSCREEN);
        }
        if window.is_some_and(|w| w.attention) {
            state.push(ATTENTION);
        }
        Info {
            title: xdg.map(|x| x.title.clone()).unwrap_or_default(),
            app_id: self
                .clients
                .get(&s.client)
                .and_then(|c| c.app_id.clone())
                .unwrap_or_default(),
            state,
            parent: xdg.and_then(|x| x.parent),
            output,
        }
    }

    /// Tells every window list what changed since it was last told: a new
    /// window's handle, a changed property, and `closed` for a window that
    /// is no more.
    pub fn update_toplevel_lists(&mut self) {
        if self.toplevels.lists.is_empty() {
            return;
        }
        let windows: Vec<SurfaceId> = self.windows().iter().map(|w| w.surface).collect();
        let mut lists = std::mem::take(&mut self.toplevels.lists);
        for ((client, list_object), list) in lists.iter_mut() {
            if list.stopped || !self.clients.contains_key(client) {
                continue;
            }
            let output = self.clients[client]
                .objects
                .iter()
                .find(|(_, o)| o.resource == Resource::Output)
                .map(|(&id, _)| id);
            // The windows that are gone.
            let gone: Vec<SurfaceId> = list
                .handles
                .keys()
                .copied()
                .filter(|s| !windows.contains(s))
                .collect();
            let mut closed = Vec::new();
            for surface in gone {
                if let Some(handle) = list.handles.remove(&surface) {
                    closed.push(handle.object);
                }
            }
            // New windows get handles before any of them tells its parent.
            let mut created = Vec::new();
            for &surface in &windows {
                if list.handles.contains_key(&surface) {
                    continue;
                }
                let c = self.clients.get_mut(client).expect("a client of a list");
                let object = c.create_server(
                    &mikros_toplevel_handle_v1::INTERFACE,
                    1,
                    Resource::ToplevelHandle(surface),
                );
                mikros_toplevel_list_v1::send_toplevel(&mut c.out, *list_object, NewId(object));
                list.handles.insert(surface, Handle { object, told: None });
                created.push(surface);
            }
            for &surface in &windows {
                let info = self.window_info(surface, output);
                let parent = info
                    .parent
                    .and_then(|p| list.handles.get(&p))
                    .map(|h| h.object);
                let Some(handle) = list.handles.get_mut(&surface) else {
                    continue;
                };
                let told = handle.told.clone().unwrap_or_default();
                if handle.told.as_ref() == Some(&info) {
                    continue;
                }
                let c = self.clients.get_mut(client).expect("a client of a list");
                let o = handle.object;
                if handle.told.is_none() || told.title != info.title {
                    mikros_toplevel_handle_v1::send_title(&mut c.out, o, &info.title);
                }
                if handle.told.is_none() || told.app_id != info.app_id {
                    mikros_toplevel_handle_v1::send_app_id(&mut c.out, o, &info.app_id);
                }
                if handle.told.is_none() || told.state != info.state {
                    let state: Vec<u8> = info.state.iter().flat_map(|v| v.to_ne_bytes()).collect();
                    mikros_toplevel_handle_v1::send_state(&mut c.out, o, &state);
                }
                if let (true, Some(out)) = (
                    handle.told.is_none() || told.output != info.output,
                    info.output,
                ) {
                    mikros_toplevel_handle_v1::send_output(&mut c.out, o, out);
                }
                if handle.told.is_none() || told.parent != info.parent {
                    mikros_toplevel_handle_v1::send_parent(&mut c.out, o, parent);
                }
                mikros_toplevel_handle_v1::send_done(&mut c.out, o);
                handle.told = Some(info);
            }
            let c = self.clients.get_mut(client).expect("a client of a list");
            for object in closed {
                mikros_toplevel_handle_v1::send_closed(&mut c.out, object);
            }
            let _ = created;
        }
        self.toplevels.lists = lists;
    }
}

#[cfg(test)]
mod tests {
    use super::super::SERVER_IDS;
    use super::super::tests::TestClient;
    use super::super::windows::tests::toplevel;
    use super::*;
    use crate::wayland::protocols::{wl_display, wl_output, wl_registry};

    /// A taskbar component, the first client, with registry 2, the output
    /// 4, the shell global 5, the seat 7, and a window list 6; windows 1 and 2.
    fn setup() -> (Server, TestClient) {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        server.next_surface = 100;
        let client = TestClient {
            id: server.connect_component(vec!["taskbar".into()], "test.taskbar".into()),
            imports: Default::default(),
        };
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &wl_output::INTERFACE, 4);
        client.bind(&mut server, 2, &mikros_shell_v1::INTERFACE, 5);
        client.bind(&mut server, 2, &wl_seat::INTERFACE, 7);
        client.send(&mut server, 5, 2, vec![Arg::NewId(NewId(6))]);
        toplevel(&mut server, 1, 10, (400, 300));
        toplevel(&mut server, 2, 11, (200, 100));
        for id in [1, 2] {
            let surface = server.surfaces.get_mut(&id).unwrap();
            surface.xdg = Some(super::super::xdg::XdgState::windowed(None, false));
        }
        (server, client)
    }

    fn events(server: &mut Server, client: &TestClient) -> Vec<(ObjectId, u16, Vec<Arg>)> {
        client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            4 => &wl_output::EVENTS,
            6 => &mikros_toplevel_list_v1::EVENTS,
            7 => &wl_seat::EVENTS,
            id if id >= SERVER_IDS => &mikros_toplevel_handle_v1::EVENTS,
            _ => &wl_registry::EVENTS,
        })
    }

    /// The events of the windows' handles, as (opcode, arguments) by object.
    fn of_handles(
        events: &[(ObjectId, u16, Vec<Arg>)],
    ) -> BTreeMap<ObjectId, Vec<(u16, &Vec<Arg>)>> {
        let mut by = BTreeMap::new();
        for (o, op, args) in events.iter().filter(|e| e.0 >= SERVER_IDS) {
            by.entry(*o).or_insert_with(Vec::new).push((*op, args));
        }
        by
    }

    #[test]
    fn tells_every_window_with_its_properties_ending_with_done() {
        let (mut server, client) = setup();
        server.update_toplevel_lists();
        let all = events(&mut server, &client);
        let new: Vec<_> = all.iter().filter(|e| (e.0, e.1) == (6, 0)).collect();
        assert_eq!(new.len(), 2);
        let handles = of_handles(&all);
        assert_eq!(handles.len(), 2);
        for events in handles.values() {
            let opcodes: Vec<u16> = events.iter().map(|e| e.0).collect();
            // title, app_id, state, output, parent, done.
            assert_eq!(opcodes, [0, 1, 2, 3, 4, 5]);
            assert_eq!(*events[1].1, [Arg::String(Some("test.taskbar".into()))]);
            assert_eq!(*events[3].1, [Arg::Object(Some(4))]);
            assert_eq!(*events[4].1, [Arg::Object(None)]);
        }
        // Nothing changed: nothing is sent.
        server.update_toplevel_lists();
        assert!(events(&mut server, &client).is_empty());
    }

    #[test]
    fn tells_the_state_when_the_focus_or_minimizing_changes() {
        let (mut server, client) = setup();
        server.update_toplevel_lists();
        events(&mut server, &client);
        server.focus_keyboard(Some(2));
        server.update_toplevel_lists();
        let all = events(&mut server, &client);
        let handles = of_handles(&all);
        assert_eq!(handles.len(), 1);
        let (_, sent) = handles.iter().next().unwrap();
        let state: Vec<u8> = [FOCUSED].iter().flat_map(|v| v.to_ne_bytes()).collect();
        assert_eq!(sent.iter().map(|e| e.0).collect::<Vec<_>>(), [2, 5]);
        assert_eq!(*sent[0].1, [Arg::Array(state)]);
    }

    #[test]
    fn closes_the_handles_of_windows_that_are_gone() {
        let (mut server, client) = setup();
        server.update_toplevel_lists();
        events(&mut server, &client);
        server.surfaces.remove(&2);
        server.update_toplevel_lists();
        let all = events(&mut server, &client);
        let handles = of_handles(&all);
        assert_eq!(handles.len(), 1);
        assert_eq!(handles.values().next().unwrap()[0].0, 6);
    }

    #[test]
    fn activating_minimizing_and_stopping() {
        let (mut server, client) = setup();
        server.update_toplevel_lists();
        let all = events(&mut server, &client);
        let handles: Vec<ObjectId> = of_handles(&all).keys().copied().collect();
        // Minimize window 1's handle: it is not shown, and its state says so.
        client.send(&mut server, handles[0], 1, vec![]);
        assert!(server.surfaces[&1].xdg.as_ref().unwrap().minimized);
        assert!(server.shown().iter().all(|s| s.surface != 1));
        server.update_toplevel_lists();
        let all = events(&mut server, &client);
        let told = of_handles(&all);
        let state: Vec<u8> = [MINIMIZED].iter().flat_map(|v| v.to_ne_bytes()).collect();
        assert_eq!(*told[&handles[0]][0].1, [Arg::Array(state)]);
        // Activate it: shown again, on top, with the focus.
        client.send(&mut server, handles[0], 0, vec![Arg::Object(Some(7))]);
        assert!(!server.surfaces[&1].xdg.as_ref().unwrap().minimized);
        assert_eq!(server.keyboard_focus(), Some(1));
        assert_eq!(server.windows().last().unwrap().surface, 1);
        // Stop.
        client.send(&mut server, 6, 0, vec![]);
        let all = events(&mut server, &client);
        assert!(all.iter().any(|e| (e.0, e.1) == (6, 1)));
        server.surfaces.remove(&2);
        server.update_toplevel_lists();
        let all = events(&mut server, &client);
        assert!(of_handles(&all).is_empty());
    }

    #[test]
    fn forgets_the_lists_of_a_client_that_disconnected() {
        let (mut server, client) = setup();
        server.update_toplevel_lists();
        assert!(!server.toplevels.lists.is_empty());
        server.disconnect(&mut super::super::tests::FakeImports::default(), client.id);
        assert!(server.toplevels.lists.is_empty());
        // Nothing is left to be told, and nothing fails.
        server.update_toplevel_lists();
    }

    #[test]
    fn only_some_components_get_a_list() {
        let mut server = Server::new();
        let client = TestClient {
            id: server.connect_component(vec!["desktop".into()], "test".into()),
            imports: Default::default(),
        };
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &mikros_shell_v1::INTERFACE, 5);
        client.send(&mut server, 5, 2, vec![Arg::NewId(NewId(6))]);
        assert_eq!(
            client.error(&mut server),
            Some(mikros_shell_v1::error::FORBIDDEN)
        );
    }
}
