//! `wl_data_device_manager` (the core protocol, MIKSPEC-0066.R16): the
//! clipboard. A client's data source offers mime types, and the one that a
//! client with the keyboard focus makes the selection is offered to the
//! client that has the keyboard focus, which gets it before it gets the
//! focus and when the selection changes while it has it; a receive from an
//! offer goes to the source's client, with the descriptor to write to. Drags
//! are not served: a drag that a client starts is cancelled.

use super::{ClientId, ProtocolError, Request, Resource, Server};
use crate::wayland::protocols::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_seat,
};
use crate::wayland::wire::{Arg, NewId, ObjectId};
use std::collections::BTreeMap;

/// The version that the compositor offers.
pub const VERSION: u32 = 3;

/// A data source of a client: the mime types it offers.
#[derive(Debug, Default)]
struct Source {
    mimes: Vec<String>,
}

/// What the clipboard keeps.
#[derive(Debug, Default)]
pub(super) struct Data {
    sources: BTreeMap<(ClientId, ObjectId), Source>,
    /// The source that is the selection, by its client and object.
    selection: Option<(ClientId, ObjectId)>,
    /// The offers that the compositor made, by their client and object, and
    /// the source that each is of.
    offers: BTreeMap<(ClientId, ObjectId), (ClientId, ObjectId)>,
}

impl Server {
    pub(super) fn data_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        let key = (request.client, request.object);
        match resource {
            Resource::DataManager => {
                match wl_data_device_manager::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    wl_data_device_manager::Request::CreateDataSource { id } => {
                        let interface = &wl_data_source::INTERFACE;
                        self.client_mut(request).create(
                            id,
                            interface,
                            request.version,
                            Resource::DataSource,
                        )?;
                        self.data
                            .sources
                            .insert((request.client, id.0), Source::default());
                    }
                    wl_data_device_manager::Request::GetDataDevice { id, seat } => {
                        let c = self.client_mut(request);
                        c.resource(request.object, seat, &wl_seat::INTERFACE)?;
                        let interface = &wl_data_device::INTERFACE;
                        c.create(id, interface, request.version, Resource::DataDevice)?;
                        // A client that has the keyboard learns the selection.
                        if self.keyboard_client() == Some(request.client) {
                            self.announce_selection_to_device(request.client, id.0);
                        }
                    }
                }
                Ok(())
            }
            Resource::DataSource => {
                match wl_data_source::Request::decode(request.opcode, args).map_err(invalid)? {
                    wl_data_source::Request::Offer { mime_type } => {
                        if let Some(source) = self.data.sources.get_mut(&key) {
                            source.mimes.push(mime_type);
                        }
                    }
                    wl_data_source::Request::SetActions { .. } => {}
                    wl_data_source::Request::Destroy => {
                        self.data.sources.remove(&key);
                        self.client_mut(request).destroy(request.object);
                        if self.data.selection == Some(key) {
                            self.data.selection = None;
                            self.announce_selection();
                        }
                    }
                }
                Ok(())
            }
            Resource::DataDevice => {
                match wl_data_device::Request::decode(request.opcode, args).map_err(invalid)? {
                    wl_data_device::Request::SetSelection { source, .. } => {
                        self.set_selection(request, source)?;
                    }
                    wl_data_device::Request::StartDrag { source, .. } => {
                        // Drags are not served: the source is cancelled.
                        if let Some(source) = source {
                            let c = self.client_mut(request);
                            wl_data_source::send_cancelled(&mut c.out, source);
                        }
                    }
                    wl_data_device::Request::Release => {
                        self.client_mut(request).destroy(request.object);
                    }
                }
                Ok(())
            }
            Resource::DataOffer => {
                match wl_data_offer::Request::decode(request.opcode, args).map_err(invalid)? {
                    wl_data_offer::Request::Receive { mime_type, fd } => {
                        // The source's client writes the data to the descriptor.
                        if let Some(&(client, source)) = self.data.offers.get(&key)
                            && let Some(c) = self.clients.get_mut(&client)
                        {
                            wl_data_source::send_send(&mut c.out, source, &mime_type, fd);
                        }
                    }
                    wl_data_offer::Request::Destroy => {
                        self.data.offers.remove(&key);
                        self.client_mut(request).destroy(request.object);
                    }
                    wl_data_offer::Request::Accept { .. }
                    | wl_data_offer::Request::Finish
                    | wl_data_offer::Request::SetActions { .. } => {}
                }
                Ok(())
            }
            _ => unreachable!("data_request handles the data device manager"),
        }
    }

    /// Makes `source` of the client of `request`, or none, the selection,
    /// when that client has the keyboard focus; a client without it is
    /// ignored, as its serial is not that of an input event that it got.
    fn set_selection(
        &mut self,
        request: Request,
        source: Option<ObjectId>,
    ) -> Result<(), ProtocolError> {
        let source = match source {
            Some(object) => {
                let c = self.client_mut(request);
                c.resource(request.object, object, &wl_data_source::INTERFACE)?;
                Some((request.client, object))
            }
            None => None,
        };
        if self.keyboard_client() != Some(request.client) {
            return Ok(());
        }
        if let Some((client, object)) = self.data.selection
            && Some((client, object)) != source
            && let Some(c) = self.clients.get_mut(&client)
        {
            wl_data_source::send_cancelled(&mut c.out, object);
        }
        self.data.selection = source;
        self.announce_selection();
        Ok(())
    }

    /// Tells the client that has the keyboard focus the selection.
    pub(super) fn announce_selection(&mut self) {
        if let Some(client) = self.keyboard_client() {
            self.announce_selection_to(client);
        }
    }

    /// Tells every data device of `client` the selection: an offer of its
    /// mime types, or none.
    pub(super) fn announce_selection_to(&mut self, client: ClientId) {
        for (device, _) in self.objects_of(client, Resource::DataDevice) {
            self.announce_selection_to_device(client, device);
        }
    }

    fn announce_selection_to_device(&mut self, client: ClientId, device: ObjectId) {
        let mimes = self
            .data
            .selection
            .and_then(|key| self.data.sources.get(&key))
            .map(|s| s.mimes.clone());
        let source = self.data.selection;
        let (Some(mimes), Some(source)) = (mimes, source) else {
            if let Some(c) = self.clients.get_mut(&client) {
                wl_data_device::send_selection(&mut c.out, device, None);
            }
            return;
        };
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        let offer = c.create_server(&wl_data_offer::INTERFACE, 3, Resource::DataOffer);
        wl_data_device::send_data_offer(&mut c.out, device, NewId(offer));
        for mime in &mimes {
            wl_data_offer::send_offer(&mut c.out, offer, mime);
        }
        wl_data_device::send_selection(&mut c.out, device, Some(offer));
        self.data.offers.insert((client, offer), source);
    }

    /// Forgets what `client` made, which disconnected: its sources, its
    /// offers, and the selection when its source was one.
    pub(super) fn forget_client_data(&mut self, client: ClientId) {
        self.data.sources.retain(|(c, _), _| *c != client);
        self.data
            .offers
            .retain(|(c, _), (s, _)| *c != client && *s != client);
        if self.data.selection.is_some_and(|(c, _)| c == client) {
            self.data.selection = None;
            self.announce_selection();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::super::windows::tests::toplevel;
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_display, wl_registry};
    use crate::wayland::wire::Fd;

    /// A client with registry 2, the data device manager 4, the seat 5, a
    /// data device 6 and a window `surface`, whose client is `id`.
    fn client(server: &mut Server) -> TestClient {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_data_device_manager::INTERFACE, 4);
        client.bind(server, 2, &wl_seat::INTERFACE, 5);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.send(
            server,
            4,
            1,
            vec![Arg::NewId(NewId(6)), Arg::Object(Some(5))],
        );
        client.send(server, 3, 0, vec![Arg::NewId(NewId(7))]);
        client
    }

    fn make_window(server: &mut Server, client: &TestClient) -> super::super::SurfaceId {
        let id = *server
            .surfaces
            .iter()
            .find(|(_, s)| s.client == client.id)
            .unwrap()
            .0;
        server.buffer_sizes.insert(100 + id, (100, 50));
        let s = server.surfaces.get_mut(&id).unwrap();
        s.role = super::super::Role::Toplevel;
        s.current.buffer = Some(100 + id);
        id
    }

    fn events(server: &mut Server, client: &TestClient) -> Vec<(ObjectId, u16, Vec<Arg>)> {
        client.events(server, &|id| match id {
            1 => &wl_display::EVENTS,
            2 => &wl_registry::EVENTS,
            5 => &wl_seat::EVENTS,
            6 => &wl_data_device::EVENTS,
            8 => &wl_data_source::EVENTS,
            id if id >= super::super::SERVER_IDS => &wl_data_offer::EVENTS,
            _ => &wl_registry::EVENTS,
        })
    }

    #[test]
    fn a_selection_goes_to_the_client_that_gets_the_keyboard_and_receive_to_the_source() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let a = client(&mut server);
        let b = client(&mut server);
        let wa = make_window(&mut server, &a);
        let wb = make_window(&mut server, &b);
        let _ = toplevel;
        // A offers text and makes it the selection while it has the keyboard.
        server.focus_keyboard(Some(wa));
        a.send(&mut server, 4, 0, vec![Arg::NewId(NewId(8))]);
        a.send(
            &mut server,
            8,
            0,
            vec![Arg::String(Some("text/plain".into()))],
        );
        a.send(&mut server, 6, 1, vec![Arg::Object(Some(8)), Arg::Uint(1)]);
        events(&mut server, &a);
        // B gets the keyboard: a data offer with the type, and the selection.
        server.focus_keyboard(Some(wb));
        let sent = events(&mut server, &b);
        let device: Vec<u16> = sent.iter().filter(|e| e.0 == 6).map(|e| e.1).collect();
        assert_eq!(device, [0, 5]);
        let offer = sent
            .iter()
            .find(|e| e.0 >= super::super::SERVER_IDS)
            .unwrap();
        assert_eq!(
            (offer.1, &offer.2[..]),
            (0, &[Arg::String(Some("text/plain".into()))][..])
        );
        // B receives: A's source is asked to send, with the descriptor.
        b.send(
            &mut server,
            offer.0,
            1,
            vec![Arg::String(Some("text/plain".into())), Arg::Fd(Fd(9))],
        );
        let asked = events(&mut server, &a);
        let send = asked.iter().find(|e| (e.0, e.1) == (8, 1)).unwrap();
        assert_eq!(send.2[0], Arg::String(Some("text/plain".into())));
    }

    #[test]
    fn a_client_without_the_keyboard_cannot_set_the_selection_and_a_new_one_cancels_the_old() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let a = client(&mut server);
        let b = client(&mut server);
        let wa = make_window(&mut server, &a);
        let _wb = make_window(&mut server, &b);
        server.focus_keyboard(Some(wa));
        // B has no keyboard: its selection is ignored.
        b.send(&mut server, 4, 0, vec![Arg::NewId(NewId(8))]);
        b.send(&mut server, 6, 1, vec![Arg::Object(Some(8)), Arg::Uint(1)]);
        assert!(server.data.selection.is_none());
        // A sets one, and then another: the first is cancelled.
        a.send(&mut server, 4, 0, vec![Arg::NewId(NewId(8))]);
        a.send(&mut server, 4, 0, vec![Arg::NewId(NewId(9))]);
        a.send(&mut server, 6, 1, vec![Arg::Object(Some(8)), Arg::Uint(1)]);
        events(&mut server, &a);
        a.send(&mut server, 6, 1, vec![Arg::Object(Some(9)), Arg::Uint(2)]);
        let sent = events(&mut server, &a);
        assert!(sent.iter().any(|e| (e.0, e.1) == (8, 2)));
        assert_eq!(server.data.selection, Some((a.id, 9)));
    }

    #[test]
    fn the_selection_ends_when_its_client_goes() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let a = client(&mut server);
        let b = client(&mut server);
        let wa = make_window(&mut server, &a);
        let wb = make_window(&mut server, &b);
        server.focus_keyboard(Some(wa));
        a.send(&mut server, 4, 0, vec![Arg::NewId(NewId(8))]);
        a.send(&mut server, 6, 1, vec![Arg::Object(Some(8)), Arg::Uint(1)]);
        server.focus_keyboard(Some(wb));
        events(&mut server, &b);
        server.disconnect(&mut super::super::tests::FakeImports::default(), a.id);
        assert!(server.data.selection.is_none());
        // B, which has the keyboard, is told that there is none.
        let sent = events(&mut server, &b);
        assert_eq!(sent.iter().filter(|e| (e.0, e.1) == (6, 5)).count(), 1);
    }

    #[test]
    fn a_drag_is_cancelled() {
        let mut server = Server::new();
        let a = client(&mut server);
        a.send(&mut server, 4, 0, vec![Arg::NewId(NewId(8))]);
        a.send(
            &mut server,
            6,
            0,
            vec![
                Arg::Object(Some(8)),
                Arg::Object(Some(7)),
                Arg::Object(None),
                Arg::Uint(1),
            ],
        );
        let sent = events(&mut server, &a);
        assert!(sent.iter().any(|e| (e.0, e.1) == (8, 2)));
    }
}
