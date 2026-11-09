//! The up-case table, by which names are compared without regard to
//! case, and the checksum it is stored with.

use alloc::vec::Vec;

/// A run of code units that map to themselves, in the compressed form
/// of the table: this value, then the run's length.
const IDENTITY_RUN: u16 = 0xFFFF;

/// The checksum of the stored table `bytes`.
pub fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |sum, &b| {
        sum.rotate_right(1).wrapping_add(u32::from(b))
    })
}

/// The up-case mapping of the 65536 UTF-16 code units, read from the
/// stored table, which may hold runs of code units that map to
/// themselves. Code units the table does not reach map to themselves.
#[derive(Default)]
pub struct Upcase(Vec<u16>);

impl Upcase {
    pub fn parse(bytes: &[u8]) -> Upcase {
        let mut map: Vec<u16> = (0..=u16::MAX).collect();
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        let (mut at, mut next) = (0usize, 0usize);
        while next < units.len() && at < map.len() {
            if units[next] == IDENTITY_RUN && next + 1 < units.len() {
                at += usize::from(units[next + 1]);
                next += 2;
                continue;
            }
            map[at] = units[next];
            at += 1;
            next += 1;
        }
        Upcase(map)
    }

    /// The up-case form of `unit`.
    pub fn up(&self, unit: u16) -> u16 {
        self.0.get(usize::from(unit)).copied().unwrap_or(unit)
    }

    /// Whether the names `a` and `b` are the same without regard to case.
    pub fn equal(&self, a: &[u16], b: &[u16]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| self.up(x) == self.up(y))
    }

    /// The hash of the name `name` that a stream extension holds.
    #[cfg(test)]
    pub fn hash(&self, name: &[u16]) -> u16 {
        let mut hash = 0u16;
        for &unit in name {
            for byte in self.up(unit).to_le_bytes() {
                hash = hash.rotate_right(1).wrapping_add(u16::from(byte));
            }
        }
        hash
    }
}
