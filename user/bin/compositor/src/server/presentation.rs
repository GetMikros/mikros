//! `wp_presentation` (MIKSPEC-0066.R16): a client asks for feedback on the
//! content update of its surface's next commit, and gets `presented` with
//! the time of the first frame that showed the surface after it, or
//! `discarded` when a later buffer replaced the content before a frame
//! showed it, or the surface was not shown. Times are those of the
//! monotonic clock. The refresh is 0, unknown, and no flags are set.

use super::{ClientId, ProtocolError, Request, Resource, Server, SurfaceId};
use crate::wayland::protocols::{wl_surface, wp_presentation, wp_presentation_feedback};
use crate::wayland::wire::{Arg, ObjectId};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;
/// `CLOCK_MONOTONIC`, whose id the clock event of the manager tells.
const CLOCK_MONOTONIC: u32 = 1;

/// The feedback objects that wait: asked for since the surface's last
/// commit, and those of commits that no frame showed yet.
#[derive(Debug, Default)]
pub(super) struct Presentation {
    asked: BTreeMap<SurfaceId, Vec<(ClientId, ObjectId)>>,
    queued: BTreeMap<SurfaceId, Vec<(ClientId, ObjectId)>>,
}

impl Presentation {
    /// The feedback that clients asked for since the surface's last commit.
    pub(super) fn take_asked(&mut self, surface: SurfaceId) -> Vec<(ClientId, ObjectId)> {
        self.asked.remove(&surface).unwrap_or_default()
    }
}

impl Server {
    pub(super) fn presentation_request(
        &mut self,
        request: Request,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match wp_presentation::Request::decode(request.opcode, args).map_err(invalid)? {
            wp_presentation::Request::Destroy => {
                self.client_mut(request).destroy(request.object);
            }
            wp_presentation::Request::Feedback { surface, callback } => {
                let c = self.client_mut(request);
                let Resource::Surface(surface_id) =
                    c.resource(request.object, surface, &wl_surface::INTERFACE)?
                else {
                    unreachable!("surfaces are wl_surface objects");
                };
                let interface = &wp_presentation_feedback::INTERFACE;
                c.create(callback, interface, request.version, Resource::Feedback)?;
                self.presentation
                    .asked
                    .entry(surface_id)
                    .or_default()
                    .push((request.client, callback.0));
            }
        }
        Ok(())
    }

    /// The commit of `surface`: `asked`, the feedback asked for since the
    /// last one, belongs to this commit's update, and `discarded` is the
    /// feedback of updates that it gives up. A commit that `replaced` what
    /// the surface shows gives up what no frame showed of it yet.
    pub(super) fn presentation_committed(
        &mut self,
        surface: SurfaceId,
        replaced: bool,
        asked: Vec<(ClientId, ObjectId)>,
        discarded: Vec<(ClientId, ObjectId)>,
    ) {
        self.discard_listed(discarded);
        if replaced {
            self.discard_feedback(surface);
        }
        if !asked.is_empty() {
            self.presentation
                .queued
                .entry(surface)
                .or_default()
                .extend(asked);
        }
    }

    /// Buffers that waited for their acquire points are now shown: the
    /// feedback of the updates that they replaced is `discarded`, and
    /// that of each surface's new content, `shown`, waits for a frame.
    pub(super) fn presentation_settled(
        &mut self,
        discarded: Vec<(ClientId, ObjectId)>,
        shown: Vec<(SurfaceId, Vec<(ClientId, ObjectId)>)>,
    ) {
        self.discard_listed(discarded);
        for (surface, feedback) in shown {
            self.discard_feedback(surface);
            if !feedback.is_empty() {
                self.presentation.queued.insert(surface, feedback);
            }
        }
    }

    /// Tells `feedback` that its content was not shown.
    pub(super) fn discard_listed(&mut self, feedback: Vec<(ClientId, ObjectId)>) {
        for (client, feedback) in feedback {
            if let Some(c) = self.clients.get_mut(&client) {
                wp_presentation_feedback::send_discarded(&mut c.out, feedback);
                c.destroy(feedback);
            }
        }
    }

    /// Tells the surface's queued feedback that its content was not shown.
    fn discard_feedback(&mut self, surface: SurfaceId) {
        for (client, feedback) in self
            .presentation
            .queued
            .remove(&surface)
            .unwrap_or_default()
        {
            if let Some(c) = self.clients.get_mut(&client) {
                wp_presentation_feedback::send_discarded(&mut c.out, feedback);
                c.destroy(feedback);
            }
        }
    }

    /// A frame that showed the windows and panels was presented at `ns`,
    /// nanoseconds of the monotonic clock: the feedback of the surfaces
    /// it showed is `presented`, and that of the others `discarded`.
    pub fn frame_presented(&mut self, ns: u64) {
        let shown: Vec<SurfaceId> = self.shown().into_iter().map(|s| s.surface).collect();
        let queued: Vec<SurfaceId> = self.presentation.queued.keys().copied().collect();
        let (secs, nanos) = (ns / 1_000_000_000, (ns % 1_000_000_000) as u32);
        for surface in queued {
            if !shown.contains(&surface) {
                self.discard_feedback(surface);
                continue;
            }
            for (client, feedback) in self
                .presentation
                .queued
                .remove(&surface)
                .unwrap_or_default()
            {
                let outputs = self.objects_of(client, Resource::Output);
                let Some(c) = self.clients.get_mut(&client) else {
                    continue;
                };
                for &(output, _) in &outputs {
                    wp_presentation_feedback::send_sync_output(&mut c.out, feedback, output);
                }
                let refresh = 0;
                let (seq_hi, seq_lo, flags) = (0, 0, 0);
                wp_presentation_feedback::send_presented(
                    &mut c.out,
                    feedback,
                    (secs >> 32) as u32,
                    secs as u32,
                    nanos,
                    refresh,
                    seq_hi,
                    seq_lo,
                    flags,
                );
                c.destroy(feedback);
            }
        }
    }

    /// Tells a client that bound the manager the clock of its times.
    pub(super) fn send_presentation_clock(&mut self, client: ClientId, manager: ObjectId) {
        if let Some(c) = self.clients.get_mut(&client) {
            wp_presentation::send_clock_id(&mut c.out, manager, CLOCK_MONOTONIC);
        }
    }

    /// Forgets the feedback of `client`, which went.
    pub(super) fn forget_client_presentation(&mut self, client: ClientId) {
        for list in [&mut self.presentation.asked, &mut self.presentation.queued] {
            list.retain(|_, feedback| {
                feedback.retain(|(c, _)| *c != client);
                !feedback.is_empty()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Role;
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::{
        wl_callback, wl_compositor, wl_display, wl_output, wl_registry,
    };
    use crate::wayland::wire::NewId;

    /// A client with the manager 4, the output 7 and the surface 5, a
    /// toplevel with a buffer.
    fn setup() -> (Server, TestClient, SurfaceId) {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &wp_presentation::INTERFACE, 4);
        client.bind(&mut server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(&mut server, 2, &wl_output::INTERFACE, 7);
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(5))]);
        let (&id, surface) = server.surfaces.iter_mut().next().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        (server, client, id)
    }

    fn feedback(server: &mut Server, client: &TestClient, id: u32) {
        let args = vec![Arg::Object(Some(5)), Arg::NewId(NewId(id))];
        client.send(server, 4, 1, args);
    }

    /// A commit of `id` that replaced what it shows, or not.
    fn commit(server: &mut Server, id: SurfaceId, replaced: bool) {
        let asked = server.presentation.take_asked(id);
        server.presentation_committed(id, replaced, asked, Vec::new());
    }

    /// The events of the manager and of the feedback objects 8 and 9.
    fn events(server: &mut Server, client: &TestClient) -> Vec<(ObjectId, u16, Vec<Arg>)> {
        client
            .events(server, &|id| match id {
                1 => &wl_display::EVENTS,
                2 => &wl_registry::EVENTS,
                4 => &wp_presentation::EVENTS,
                7 => &wl_output::EVENTS,
                8 | 9 => &wp_presentation_feedback::EVENTS,
                _ => &wl_callback::EVENTS,
            })
            .into_iter()
            .filter(|e| matches!(e.0, 4 | 8 | 9))
            .collect()
    }

    #[test]
    fn tells_the_clock_when_the_manager_is_bound() {
        let (mut server, client, _) = setup();
        let sent = events(&mut server, &client);
        assert_eq!(sent, [(4, 0, vec![Arg::Uint(CLOCK_MONOTONIC)])]);
    }

    #[test]
    fn a_frame_that_shows_the_surface_presents_the_feedback_of_its_commit() {
        let (mut server, client, id) = setup();
        events(&mut server, &client);
        feedback(&mut server, &client, 8);
        // Asked for after the commit, it is of the next one.
        assert!(events(&mut server, &client).is_empty());
        commit(&mut server, id, false);
        server.frame_presented(5_000_000_123 + (3 << 32) * 1_000_000_000);
        let sent = events(&mut server, &client);
        let kinds: Vec<(ObjectId, u16)> = sent.iter().map(|e| (e.0, e.1)).collect();
        assert_eq!(kinds, [(8, 0), (8, 1)]);
        assert_eq!(
            sent[1].2,
            [
                Arg::Uint(3),
                Arg::Uint(5),
                Arg::Uint(123),
                Arg::Uint(0),
                Arg::Uint(0),
                Arg::Uint(0),
                Arg::Uint(0)
            ]
        );
        // Told once.
        server.frame_presented(6_000_000_000);
        assert!(events(&mut server, &client).is_empty());
    }

    #[test]
    fn a_new_buffer_discards_the_feedback_that_no_frame_showed() {
        let (mut server, client, id) = setup();
        events(&mut server, &client);
        feedback(&mut server, &client, 8);
        commit(&mut server, id, false);
        feedback(&mut server, &client, 9);
        commit(&mut server, id, true);
        let sent = events(&mut server, &client);
        assert_eq!(
            sent.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(),
            [(8, 2)]
        );
        server.frame_presented(1_000_000_000);
        let sent = events(&mut server, &client);
        assert_eq!(sent.iter().filter(|e| (e.0, e.1) == (9, 1)).count(), 1);
    }

    #[test]
    fn feedback_of_a_surface_that_is_not_shown_is_discarded() {
        let (mut server, client, id) = setup();
        events(&mut server, &client);
        feedback(&mut server, &client, 8);
        commit(&mut server, id, false);
        server.surfaces.get_mut(&id).unwrap().current.buffer = None;
        server.frame_presented(1_000_000_000);
        let sent = events(&mut server, &client);
        assert_eq!(
            sent.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(),
            [(8, 2)]
        );
    }
}
