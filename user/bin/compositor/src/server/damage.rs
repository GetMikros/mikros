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

/// The rectangle that holds `a` and `b`.
fn union(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.x.saturating_add(a.width).max(b.x.saturating_add(b.width));
    let y1 =
        a.y.saturating_add(a.height)
            .max(b.y.saturating_add(b.height));
    Rect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

fn area(r: Rect) -> i64 {
    i64::from(r.width.max(0)) * i64::from(r.height.max(0))
}

/// `rects` with every pair joined that overlaps or whose joint rectangle
/// adds at most `extra` pixels to the two: drawing an area costs about a
/// draw for each layer over it however small it is, so a few more pixels
/// in one area are cheaper than another area.
pub fn coalesce(rects: &[Rect], extra: i64) -> Vec<Rect> {
    let mut list: Vec<Rect> = rects.iter().copied().filter(|r| area(*r) > 0).collect();
    // Past this many the pairs are too many to compare.
    if list.len() > 128 {
        return join(&list, 1);
    }
    loop {
        let mut found = None;
        'pairs: for i in 0..list.len() {
            for j in i + 1..list.len() {
                let added = area(union(list[i], list[j])) - area(list[i]) - area(list[j]);
                if added <= extra {
                    found = Some((i, j));
                    break 'pairs;
                }
            }
        }
        let Some((i, j)) = found else {
            return list;
        };
        list[i] = union(list[i], list[j]);
        list.swap_remove(j);
    }
}

/// `rects` as at most `most` rectangles that hold all of them, as the
/// display takes no more in a commit: while there are more, the two whose
/// common rectangle adds least are joined, and a list too long to
/// compare is the one rectangle that holds it.
pub fn join(rects: &[Rect], most: usize) -> Vec<Rect> {
    let mut list = rects.to_vec();
    let most = most.max(1);
    if list.len() <= most {
        return list;
    }
    if list.len() > 128 {
        let all = list.iter().copied().reduce(union);
        return all.into_iter().collect();
    }
    while list.len() > most {
        let mut best = (0, 1, i64::MAX);
        for i in 0..list.len() {
            for j in i + 1..list.len() {
                let added = area(union(list[i], list[j])) - area(list[i]) - area(list[j]);
                if added < best.2 {
                    best = (i, j, added);
                }
            }
        }
        let joined = union(list[best.0], list[best.1]);
        list.swap_remove(best.1);
        list[best.0] = joined;
    }
    list
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
            let Some(&buffer) = self.buffer_sizes.get(&shown.buffer) else {
                continue;
            };
            let rect = Rect {
                x: shown.x,
                y: shown.y,
                width: shown.width,
                height: shown.height,
            };
            let stretched = (shown.width, shown.height) != (buffer.0 as i32, buffer.1 as i32)
                || shown.source.is_some();
            now.insert(shown.surface, rect);
            match self.drawn.get(&shown.surface) {
                // A stretched buffer is drawn again whole when it changes.
                Some(&old) if old == rect && stretched => {
                    if self.committed.contains_key(&shown.surface) {
                        damage.push(rect);
                    }
                }
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
        damage.append(&mut self.restacked);
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

    fn r(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn coalesces_rectangles_that_overlap_or_add_little() {
        // The old and new place of a pointer, which overlap, are one area.
        let pointer = [r(100, 100, 24, 24), r(110, 105, 24, 24)];
        assert_eq!(coalesce(&pointer, 0), vec![r(100, 100, 34, 29)]);
        // Two frames of that make one.
        let frames = [
            r(100, 100, 24, 24),
            r(110, 105, 24, 24),
            r(120, 110, 24, 24),
            r(130, 115, 24, 24),
        ];
        assert_eq!(coalesce(&frames, 100), vec![r(100, 100, 54, 39)]);
        // Two windows' places far apart stay two areas.
        let far = [r(0, 0, 300, 200), r(600, 400, 300, 200)];
        assert_eq!(coalesce(&far, 65_536).len(), 2);
        // Near each other they are one when the joint rectangle adds
        // little.
        let near = [r(0, 0, 100, 100), r(110, 0, 100, 100)];
        assert_eq!(coalesce(&near, 65_536), vec![r(0, 0, 210, 100)]);
        assert_eq!(coalesce(&near, 0).len(), 2);
        // Empty rectangles go.
        assert!(coalesce(&[r(5, 5, 0, 10)], 0).is_empty());
    }

    #[test]
    fn joins_a_list_that_is_too_long_into_rectangles_that_hold_all_of_it() {
        let few = [r(0, 0, 10, 10), r(50, 50, 10, 10)];
        assert_eq!(join(&few, 16), few.to_vec());
        // Twenty small rectangles, two near each other: at most 16 remain,
        // and each original lies within one of them.
        let many: Vec<Rect> = (0..20).map(|i| r(i * 100, 0, 10, 10)).collect();
        let joined = join(&many, 16);
        assert_eq!(joined.len(), 16);
        for m in &many {
            assert!(joined.iter().any(|j| {
                j.x <= m.x
                    && j.y <= m.y
                    && j.x + j.width >= m.x + m.width
                    && j.y + j.height >= m.y + m.height
            }));
        }
        // The pair that adds least is joined: the two nearest.
        let near = [r(0, 0, 10, 10), r(10, 0, 10, 10), r(500, 500, 10, 10)];
        let joined = join(&near, 2);
        assert!(joined.contains(&r(0, 0, 20, 10)) && joined.contains(&r(500, 500, 10, 10)));
        // A list too long to compare is the one rectangle that holds it.
        let huge: Vec<Rect> = (0..200).map(|i| r(i, i, 1, 1)).collect();
        assert_eq!(join(&huge, 16), vec![r(0, 0, 200, 200)]);
    }

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
