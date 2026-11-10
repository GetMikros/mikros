//! The allocation bitmap, one bit for each cluster of the heap, held in
//! memory whole.

use alloc::vec::Vec;

pub(crate) struct Bitmap {
    /// The clusters that hold the bitmap on the volume, in order.
    pub(crate) clusters: Vec<u32>,
    /// The bitmap's bytes, as many as its clusters hold.
    pub(crate) bytes: Vec<u8>,
    /// How many clusters the heap has.
    count: u32,
    /// How many of them are free.
    pub(crate) free: u32,
}

impl Bitmap {
    /// The bitmap of a heap of `count` clusters, which `bytes`, read from
    /// the clusters `clusters`, holds a bit for each of.
    pub(crate) fn new(clusters: Vec<u32>, bytes: Vec<u8>, count: u32) -> Bitmap {
        let used: u32 = (0..count).map(|i| u32::from(bit(&bytes, i))).sum();
        Bitmap {
            clusters,
            bytes,
            count,
            free: count - used,
        }
    }

    /// An empty bitmap, until the volume's is read.
    pub(crate) fn empty() -> Bitmap {
        Bitmap::new(Vec::new(), Vec::new(), 0)
    }

    /// Whether cluster `cluster` of the heap is free.
    pub(crate) fn is_free(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster - 2 < self.count && !bit(&self.bytes, cluster - 2)
    }

    /// Marks cluster `cluster` of the heap as in use or free, and gives
    /// the index of the byte that holds its bit.
    pub(crate) fn set(&mut self, cluster: u32, used: bool) -> usize {
        let i = cluster - 2;
        let byte = (i / 8) as usize;
        if bit(&self.bytes, i) != used {
            self.bytes[byte] ^= 1 << (i % 8);
            match used {
                true => self.free -= 1,
                false => self.free += 1,
            }
        }
        byte
    }

    /// The first cluster of `len` free clusters in a row, looking from
    /// cluster `from` on and then from the start of the heap.
    pub(crate) fn run(&self, from: u32, len: u32) -> Option<u32> {
        if len == 0 || len > self.free {
            return None;
        }
        let start = self.index(from);
        [
            (start, self.count),
            (0, start.saturating_add(len - 1).min(self.count)),
        ]
        .into_iter()
        .find_map(|(lo, hi)| {
            let mut run = 0;
            for i in lo..hi {
                run = if bit(&self.bytes, i) { 0 } else { run + 1 };
                if run == len {
                    return Some(i + 3 - len);
                }
            }
            None
        })
    }

    /// `count` free clusters, the first ones from cluster `from` on and
    /// then from the start of the heap, or `None` when fewer are free.
    pub(crate) fn take(&self, from: u32, count: u32) -> Option<Vec<u32>> {
        if count > self.free {
            return None;
        }
        let start = self.index(from);
        let found = (start..self.count)
            .chain(0..start)
            .filter(|&i| !bit(&self.bytes, i))
            .take(count as usize)
            .map(|i| i + 2);
        Some(found.collect())
    }

    /// The index in the heap of cluster `cluster`, or 0 for one outside.
    fn index(&self, cluster: u32) -> u32 {
        match cluster.checked_sub(2) {
            Some(i) if i < self.count => i,
            _ => 0,
        }
    }
}

/// The bit of heap index `i` in `bytes`.
fn bit(bytes: &[u8], i: u32) -> bool {
    bytes[(i / 8) as usize] & (1 << (i % 8)) != 0
}
