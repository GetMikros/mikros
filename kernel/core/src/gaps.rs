//! Free gaps of an address range, ordered by length, from which a start
//! for a range of a given length can be chosen uniformly among all the
//! places it fits, in time logarithmic in the number of gaps.
//!
//! The gaps are kept in an AVL tree ordered by length, then start. Each
//! node also holds the number of gaps in its subtree and the sum of their
//! lengths, so that the number of places a range of `n` units fits in the
//! gaps of at least `n` units, the sum of `len - n + 1` over them, can be
//! read off a path from the root. Nodes live in an arena; room for them is
//! reserved beforehand, so that inserting and removing never allocate.

use crate::status::Error;
use alloc::vec::Vec;

/// No node.
const NIL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
struct Node {
    start: u64,
    len: u64,
    left: u32,
    right: u32,
    height: u32,
    /// The number of gaps in the subtree and the sum of their lengths.
    count: u64,
    sum: u64,
}

/// The size of the room one gap takes, for charging.
pub const GAP_SIZE: usize = size_of::<Node>() + size_of::<u32>();

#[derive(Debug)]
pub struct Gaps {
    nodes: Vec<Node>,
    /// The unused slots of `nodes`, with room for every slot.
    free: Vec<u32>,
    root: u32,
}

impl Default for Gaps {
    fn default() -> Self {
        Self::new()
    }
}

impl Gaps {
    pub const fn new() -> Gaps {
        Gaps {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NIL,
        }
    }

    /// The number of gaps.
    pub fn len(&self) -> u64 {
        self.count(self.root)
    }

    pub fn is_empty(&self) -> bool {
        self.root == NIL
    }

    /// Makes room for `additional` more gaps than there are now, or fails
    /// with `NoMemory`, changing nothing that `insert` and `remove` see.
    pub fn reserve(&mut self, additional: usize) -> Result<(), Error> {
        let live = self.nodes.len() - self.free.len();
        let wanted = live.checked_add(additional).ok_or(Error::NoMemory)?;
        if wanted > u32::MAX as usize {
            return Err(Error::NoMemory);
        }
        let more = wanted.saturating_sub(self.nodes.len());
        self.nodes.try_reserve(more).map_err(|_| Error::NoMemory)?;
        let slots = self.nodes.len() + more;
        self.free
            .try_reserve(slots - self.free.len())
            .map_err(|_| Error::NoMemory)
    }

    /// Removes every gap, keeping the room.
    pub fn clear(&mut self) {
        self.free.clear();
        self.free.extend((0..self.nodes.len() as u32).rev());
        self.root = NIL;
    }

    /// Adds the gap of `len` units from `start`, for which room was
    /// reserved. `len` is not zero and no gap starts at `start`.
    pub fn insert(&mut self, start: u64, len: u64) {
        debug_assert!(len > 0);
        let node = Node {
            start,
            len,
            left: NIL,
            right: NIL,
            height: 1,
            count: 1,
            sum: len,
        };
        let i = match self.free.pop() {
            Some(i) => {
                self.nodes[i as usize] = node;
                i
            }
            None => {
                assert!(
                    self.nodes.len() < self.nodes.capacity(),
                    "no room was reserved for a gap"
                );
                self.nodes.push(node);
                (self.nodes.len() - 1) as u32
            }
        };
        self.root = self.insert_at(self.root, i);
    }

    /// Removes the gap of `len` units from `start`, which exists.
    pub fn remove(&mut self, start: u64, len: u64) {
        let mut removed = NIL;
        self.root = self.remove_at(self.root, (len, start), &mut removed);
        assert!(removed != NIL, "removed a gap that does not exist");
        // `reserve` made room for every slot.
        debug_assert!(self.free.len() < self.free.capacity());
        self.free.push(removed);
    }

    /// The number of places a range of `n` units fits: the sum of
    /// `len - n + 1` over the gaps of at least `n` units. `n` is not zero.
    pub fn places(&self, n: u64) -> u64 {
        debug_assert!(n > 0);
        let mut total = 0;
        let mut at = self.root;
        while at != NIL {
            let node = self.nodes[at as usize];
            if node.len >= n {
                // The right subtree holds only gaps at least as long.
                total += self.weight(node.right, n) + (node.len - n + 1);
                at = node.left;
            } else {
                at = node.right;
            }
        }
        total
    }

    /// The start of place `r` of a range of `n` units, for `r` below
    /// `places(n)`. Each place has its own `r`, so a uniform `r` gives a
    /// uniform place.
    pub fn place(&self, n: u64, mut r: u64) -> u64 {
        debug_assert!(n > 0);
        let mut at = self.root;
        while at != NIL {
            let node = self.nodes[at as usize];
            if node.len < n {
                at = node.right;
                continue;
            }
            let right = self.weight(node.right, n);
            if r < right {
                at = node.right;
                continue;
            }
            r -= right;
            let own = node.len - n + 1;
            if r < own {
                return node.start + r;
            }
            r -= own;
            at = node.left;
        }
        panic!("place {r} is beyond the places a range fits");
    }

    /// The places of a range of `n` units in subtree `at`, all of whose
    /// gaps are at least `n` units long.
    fn weight(&self, at: u32, n: u64) -> u64 {
        if at == NIL {
            return 0;
        }
        let node = &self.nodes[at as usize];
        node.sum - (n - 1) * node.count
    }

    fn count(&self, at: u32) -> u64 {
        if at == NIL {
            0
        } else {
            self.nodes[at as usize].count
        }
    }

    fn sum(&self, at: u32) -> u64 {
        if at == NIL {
            0
        } else {
            self.nodes[at as usize].sum
        }
    }

    fn height(&self, at: u32) -> u32 {
        if at == NIL {
            0
        } else {
            self.nodes[at as usize].height
        }
    }

    fn key(&self, at: u32) -> (u64, u64) {
        let node = &self.nodes[at as usize];
        (node.len, node.start)
    }

    /// Recomputes the height, count and sum of `at` from its children.
    fn update(&mut self, at: u32) {
        let node = self.nodes[at as usize];
        let (l, r) = (node.left, node.right);
        let height = 1 + self.height(l).max(self.height(r));
        let count = 1 + self.count(l) + self.count(r);
        let sum = node.len + self.sum(l) + self.sum(r);
        let node = &mut self.nodes[at as usize];
        node.height = height;
        node.count = count;
        node.sum = sum;
    }

    fn rotate_right(&mut self, at: u32) -> u32 {
        let top = self.nodes[at as usize].left;
        self.nodes[at as usize].left = self.nodes[top as usize].right;
        self.nodes[top as usize].right = at;
        self.update(at);
        self.update(top);
        top
    }

    fn rotate_left(&mut self, at: u32) -> u32 {
        let top = self.nodes[at as usize].right;
        self.nodes[at as usize].right = self.nodes[top as usize].left;
        self.nodes[top as usize].left = at;
        self.update(at);
        self.update(top);
        top
    }

    /// Updates `at` and restores the AVL balance there, returning the
    /// subtree's new root.
    fn balance(&mut self, at: u32) -> u32 {
        self.update(at);
        let (l, r) = (self.nodes[at as usize].left, self.nodes[at as usize].right);
        let lean = self.height(l) as i64 - self.height(r) as i64;
        if lean > 1 {
            let ll = self.nodes[l as usize].left;
            let lr = self.nodes[l as usize].right;
            if self.height(lr) > self.height(ll) {
                self.nodes[at as usize].left = self.rotate_left(l);
            }
            return self.rotate_right(at);
        }
        if lean < -1 {
            let rl = self.nodes[r as usize].left;
            let rr = self.nodes[r as usize].right;
            if self.height(rl) > self.height(rr) {
                self.nodes[at as usize].right = self.rotate_right(r);
            }
            return self.rotate_left(at);
        }
        at
    }

    fn insert_at(&mut self, at: u32, new: u32) -> u32 {
        if at == NIL {
            return new;
        }
        if self.key(new) < self.key(at) {
            let left = self.insert_at(self.nodes[at as usize].left, new);
            self.nodes[at as usize].left = left;
        } else {
            let right = self.insert_at(self.nodes[at as usize].right, new);
            self.nodes[at as usize].right = right;
        }
        self.balance(at)
    }

    /// Removes the node with `key` from subtree `at`, storing its slot in
    /// `removed`, and returns the subtree's new root.
    fn remove_at(&mut self, at: u32, key: (u64, u64), removed: &mut u32) -> u32 {
        if at == NIL {
            return NIL;
        }
        let node = self.nodes[at as usize];
        let here = self.key(at);
        if key < here {
            let left = self.remove_at(node.left, key, removed);
            self.nodes[at as usize].left = left;
        } else if key > here {
            let right = self.remove_at(node.right, key, removed);
            self.nodes[at as usize].right = right;
        } else {
            *removed = at;
            if node.left == NIL {
                return node.right;
            }
            if node.right == NIL {
                return node.left;
            }
            // The smallest node of the right subtree takes this place.
            let mut next = NIL;
            let right = self.take_min(node.right, &mut next);
            self.nodes[next as usize].left = node.left;
            self.nodes[next as usize].right = right;
            return self.balance(next);
        }
        self.balance(at)
    }

    /// Unlinks the smallest node of subtree `at`, storing it in `min`, and
    /// returns the subtree's new root.
    fn take_min(&mut self, at: u32, min: &mut u32) -> u32 {
        let left = self.nodes[at as usize].left;
        if left == NIL {
            *min = at;
            return self.nodes[at as usize].right;
        }
        let left = self.take_min(left, min);
        self.nodes[at as usize].left = left;
        self.balance(at)
    }

    /// Checks the order, balance, heights, counts and sums of every node,
    /// and returns the gaps in order, for tests and fuzzing.
    pub fn check(&self) -> Vec<(u64, u64)> {
        fn walk(g: &Gaps, at: u32, out: &mut Vec<(u64, u64)>) -> (u32, u64, u64) {
            if at == NIL {
                return (0, 0, 0);
            }
            let node = g.nodes[at as usize];
            let (lh, lc, ls) = walk(g, node.left, out);
            out.push((node.start, node.len));
            let (rh, rc, rs) = walk(g, node.right, out);
            assert!(lh.abs_diff(rh) <= 1, "unbalanced");
            assert_eq!(node.height, 1 + lh.max(rh));
            assert_eq!(node.count, 1 + lc + rc);
            assert_eq!(node.sum, node.len + ls + rs);
            (node.height, node.count, node.sum)
        }
        let mut out = Vec::new();
        walk(self, self.root, &mut out);
        assert!(out.windows(2).all(|w| (w[0].1, w[0].0) < (w[1].1, w[1].0)));
        assert_eq!(
            out.len() + self.free.len(),
            self.nodes.len(),
            "slots leaked"
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::vec;

    /// A small deterministic generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Every place of a range of `n` units in `gaps`, in no order.
    fn all_places(gaps: &BTreeSet<(u64, u64)>, n: u64) -> Vec<u64> {
        let mut out = Vec::new();
        for &(start, len) in gaps {
            if len >= n {
                out.extend(start..=start + len - n);
            }
        }
        out
    }

    /// Checks that the places of every length match the model, and that
    /// each place has exactly one `r`.
    fn agrees(g: &Gaps, model: &BTreeSet<(u64, u64)>) {
        let mut listed: Vec<_> = g.check().into_iter().collect();
        listed.sort();
        assert_eq!(listed, model.iter().copied().collect::<Vec<_>>());
        let longest = model.iter().map(|&(_, l)| l).max().unwrap_or(0);
        for n in 1..=longest + 1 {
            let mut expected = all_places(model, n);
            assert_eq!(g.places(n), expected.len() as u64, "places of {n}");
            let mut picked: Vec<_> = (0..g.places(n)).map(|r| g.place(n, r)).collect();
            expected.sort();
            picked.sort();
            assert_eq!(picked, expected, "places of {n}");
        }
    }

    #[test]
    fn counts_and_picks_every_place_once() {
        let mut g = Gaps::new();
        let mut model = BTreeSet::new();
        g.reserve(4).unwrap();
        for (start, len) in [(0, 3), (10, 1), (20, 7), (40, 3)] {
            g.insert(start, len);
            model.insert((start, len));
        }
        agrees(&g, &model);
        assert_eq!(g.places(3), 1 + 5 + 1);
        assert_eq!(g.places(8), 0);
        g.remove(20, 7);
        model.remove(&(20, 7));
        agrees(&g, &model);
    }

    #[test]
    fn stays_balanced_and_consistent_under_random_changes() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut g = Gaps::new();
        let mut model: BTreeSet<(u64, u64)> = BTreeSet::new();
        for step in 0..3000 {
            if model.is_empty() || rng.next() % 3 != 0 {
                // Gaps of a few repeated lengths, as fragmentation makes.
                let start = rng.next() % 100_000;
                let len = 1 + rng.next() % 9;
                if model.iter().any(|&(s, _)| s == start) {
                    continue;
                }
                g.reserve(1).unwrap();
                g.insert(start, len);
                model.insert((start, len));
            } else {
                let i = (rng.next() % model.len() as u64) as usize;
                let (start, len) = *model.iter().nth(i).unwrap();
                g.remove(start, len);
                model.remove(&(start, len));
            }
            if step % 300 == 0 {
                agrees(&g, &model);
            }
        }
        agrees(&g, &model);
        // The height stays logarithmic.
        let n = model.len() as f64;
        assert!(f64::from(g.height(g.root)) <= 1.45 * (n + 2.0).log2());
    }

    #[test]
    fn picks_uniformly_in_fragmented_space() {
        // Many short gaps and a few long ones: a range of 50 units fits
        // only in the long ones, each place equally often.
        let mut g = Gaps::new();
        g.reserve(1004).unwrap();
        for i in 0..1000 {
            g.insert(i * 100, 1 + i % 7);
        }
        for (start, len) in [(200_000, 60), (300_000, 51), (400_000, 50), (500_000, 99)] {
            g.insert(start, len);
        }
        let places = g.places(50);
        assert_eq!(places, 11 + 2 + 1 + 50);
        let mut rng = Rng(12345);
        let mut hits = std::collections::BTreeMap::new();
        let draws = places * 2000;
        for _ in 0..draws {
            *hits.entry(g.place(50, rng.next() % places)).or_insert(0u64) += 1;
        }
        assert_eq!(hits.len() as u64, places);
        for (&at, &n) in &hits {
            assert!(
                (1500..2500).contains(&n),
                "place {at} drawn {n} times of {draws}"
            );
        }
    }

    #[test]
    fn reuses_room_and_never_allocates_after_reserving() {
        let mut g = Gaps::new();
        g.reserve(2).unwrap();
        let (cap, free_cap) = (g.nodes.capacity(), g.free.capacity());
        g.insert(0, 4);
        g.insert(8, 4);
        g.remove(0, 4);
        g.insert(16, 2);
        g.remove(8, 4);
        g.remove(16, 2);
        g.insert(3, 1);
        assert_eq!((g.nodes.capacity(), g.free.capacity()), (cap, free_cap));
        g.clear();
        assert!(g.is_empty());
        assert_eq!(g.check(), vec![]);
        g.insert(5, 5);
        assert_eq!(g.place(5, 0), 5);
    }
}
