//! The damage of the output (MIKSPEC-0066.R17): what changed since the
//! last frame, in output coordinates. A window or panel that appears,
//! disappears, moves or changes size damages its old and new area; one
//! that stays damages what its client damaged with `damage` or
//! `damage_buffer` at its commits, or all of it when a commit brought a
//! new buffer without any damage.

use super::{Rect, Server, SurfaceId};

/// The rectangle of `rect`, in a surface whose corner lies at `origin`,
/// within a surface of `size`, in output coordinates, or None when
/// nothing of it lies in the surface.
fn on_output(rect: Rect, origin: (i32, i32), size: (i32, i32)) -> Option<Rect> {
    let (x0, y0) = (rect.x.max(0), rect.y.max(0));
    let x1 = rect.x.saturating_add(rect.width).min(size.0);
    let y1 = rect.y.saturating_add(rect.height).min(size.1);
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: origin.0 + x0,
        y: origin.1 + y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

impl Server {
    /// Records the damage of a commit of `surface`, in its own
    /// coordinates: what its client damaged, or all of it for a new
    /// buffer without damage. The damage of a commit whose buffer `waits`
    /// for its acquire point goes with that buffer.
    pub(super) fn commit_damage(&mut self, surface: SurfaceId, new_buffer: bool, waits: bool) {
        let Some(s) = self.surfaces.get_mut(&surface) else {
            return;
        };
        let state = &s.current;
        let mut rects: Vec<Rect> = state
            .damage
            .iter()
            .chain(&state.buffer_damage)
            .copied()
            .collect();
        if rects.is_empty() && new_buffer {
            rects.push(Rect {
                x: 0,
                y: 0,
                width: i32::MAX,
                height: i32::MAX,
            });
        }
        if let (true, Some(waiting)) = (waits, s.waiting.last_mut()) {
            waiting.damage.extend(rects);
        } else if !rects.is_empty() {
            self.committed.entry(surface).or_default().extend(rects);
        }
    }

    /// The damage since the last call, in output coordinates.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        let mut damage = Vec::new();
        let mut now = std::collections::BTreeMap::new();
        for shown in self.shown() {
            let Some(&(width, height)) = self.buffer_sizes.get(&shown.buffer) else {
                continue;
            };
            let rect = Rect {
                x: shown.x,
                y: shown.y,
                width: width as i32,
                height: height as i32,
            };
            now.insert(shown.surface, rect);
            match self.drawn.get(&shown.surface) {
                Some(&old) if old == rect => {
                    let size = (rect.width, rect.height);
                    let committed = self.committed.get(&shown.surface).into_iter().flatten();
                    damage.extend(committed.filter_map(|&r| on_output(r, (rect.x, rect.y), size)));
                }
                old => damage.extend(old.into_iter().copied().chain([rect])),
            }
        }
        // What is no longer shown.
        damage.extend(
            self.drawn
                .iter()
                .filter(|(id, _)| !now.contains_key(id))
                .map(|(_, &old)| old),
        );
        self.drawn = now;
        self.committed.clear();
        damage
    }

    /// Whether the cursor surface `surface` was committed with damage since
    /// the last call, which forgets that.
    pub fn take_cursor_damage(&mut self, surface: SurfaceId) -> bool {
        self.committed.remove(&surface).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::super::{Role, Server};
    use super::*;
    use crate::wayland::protocols::wl_compositor;
    use crate::wayland::wire::{Arg, NewId};

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// A client whose surface 4 is a toplevel of 100 by 50 at the output's
    /// corner, with the buffer 99.
    fn with_window(server: &mut Server) -> (TestClient, SurfaceId) {
        let client = TestClient::new(server);
        client.send(server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(server, 2, &wl_compositor::INTERFACE, 3);
        client.send(server, 3, 0, vec![Arg::NewId(NewId(4))]);
        let (&id, surface) = server.surfaces.iter_mut().next().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        (client, id)
    }

    #[test]
    fn damages_surfaces_that_appear_and_disappear() {
        let mut server = Server::new();
        let (_, id) = with_window(&mut server);
        assert_eq!(server.take_damage(), [rect(0, 0, 100, 50)]);
        assert!(server.take_damage().is_empty());
        server.surfaces.get_mut(&id).unwrap().current.buffer = None;
        assert_eq!(server.take_damage(), [rect(0, 0, 100, 50)]);
    }

    #[test]
    fn damages_what_clients_damaged_within_their_surfaces() {
        let mut server = Server::new();
        let (client, id) = with_window(&mut server);
        server.take_damage();
        // damage_buffer of 90,40 120x30, then commit.
        client.send(
            &mut server,
            4,
            9,
            Vec::from([90, 40, 120, 30].map(Arg::Int)),
        );
        client.send(&mut server, 4, 6, vec![]);
        assert_eq!(server.take_damage(), [rect(90, 40, 10, 10)]);
        // A new buffer without damage damages the whole surface.
        let state = &mut server.surfaces.get_mut(&id).unwrap().current;
        state.damage.clear();
        state.buffer_damage.clear();
        server.commit_damage(id, true, false);
        assert_eq!(server.take_damage(), [rect(0, 0, 100, 50)]);
    }
}
