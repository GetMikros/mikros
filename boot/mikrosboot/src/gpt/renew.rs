//! New GUIDs for the boot disk's GUID partition table (MIKSPEC-0004.R41):
//! both tables read whole, the disk and every used entry given a new
//! random GUID of version 4, the same in both tables, and the CRC32
//! values made again, for writing back over the old bytes.

use super::{Raw, raw};
use crate::crc::crc32;
use crate::disk::{Disk, IoError};
use alloc::vec::Vec;

/// Both tables of a disk, as they lie on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tables {
    pub primary: Raw,
    pub backup: Raw,
}

/// A partition GUID before and after the renewal, as GPT stores them.
pub type Renamed = ([u8; 16], [u8; 16]);

/// The tables of the disk whose last block is `last_lba`, when the
/// primary table at LBA 1 and the backup table at `last_lba` are both
/// valid and hold the same entries.
pub fn read(
    disk: &mut impl Disk,
    block_size: u32,
    last_lba: u64,
) -> Result<Option<Tables>, IoError> {
    if block_size < 512 || last_lba < 2 {
        return Ok(None);
    }
    let primary = raw(disk, block_size, last_lba, 1)?;
    let backup = raw(disk, block_size, last_lba, last_lba)?;
    Ok(match (primary, backup) {
        (Some(primary), Some(backup))
            if primary.entries == backup.entries && primary.entry_size == backup.entry_size =>
        {
            Some(Tables { primary, backup })
        }
        _ => None,
    })
}

/// A new GUID of version 4 from the 16 bytes that `random` fills, in the
/// byte order GPT stores GUIDs in: the version is the high nibble of the
/// third field, which is little-endian, and the variant the top bits of
/// the fourth.
fn guid(random: &mut dyn FnMut(&mut [u8]) -> bool) -> Option<[u8; 16]> {
    let mut g = [0u8; 16];
    if !random(&mut g) {
        return None;
    }
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    Some(g)
}

/// `tables` with a new disk GUID in both headers and a new partition GUID
/// for each entry with a non-zero type GUID, the same at the same index
/// in both arrays, and with the CRC32 values of the arrays and headers
/// made again; and the partition GUIDs before and after. None when
/// `random` fails.
pub fn renewed(
    tables: &Tables,
    random: &mut dyn FnMut(&mut [u8]) -> bool,
) -> Option<(Tables, Vec<Renamed>)> {
    let mut new = tables.clone();
    let disk_guid = guid(random)?;
    let size = tables.primary.entry_size;
    let mut renamed = Vec::new();
    for (i, entry) in tables.primary.entries.chunks_exact(size).enumerate() {
        if entry[..16] == [0; 16] {
            continue;
        }
        let old: [u8; 16] = entry[16..32].try_into().expect("16 bytes");
        let fresh = guid(random)?;
        for table in [&mut new.primary, &mut new.backup] {
            table.entries[i * size + 16..i * size + 32].copy_from_slice(&fresh);
        }
        renamed.push((old, fresh));
    }
    for table in [&mut new.primary, &mut new.backup] {
        table.header[56..72].copy_from_slice(&disk_guid);
        let entries = crc32(&table.entries);
        table.header[88..92].copy_from_slice(&entries.to_le_bytes());
        table.header[16..20].fill(0);
        let header = crc32(&table.header[..table.header_size]);
        table.header[16..20].copy_from_slice(&header.to_le_bytes());
    }
    Some((new, renamed))
}

/// The writes that put `tables` on the disk, as byte offset and bytes:
/// the backup table's entries and header, then the primary table's, so
/// that a valid table is left whenever a write fails.
pub fn writes(tables: &Tables) -> [(u64, &[u8]); 4] {
    let (p, b) = (&tables.primary, &tables.backup);
    [
        (b.entries_at, &b.entries),
        (b.header_at, &b.header),
        (p.entries_at, &p.entries),
        (p.header_at, &p.header),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::tests::{BLOCK, BLOCKS, disk, two_roots};
    use super::super::{ROOT_TYPE, partitions};
    use super::*;
    use alloc::vec;

    /// A source of random bytes that counts up from `next`.
    fn counting(next: &mut u8) -> impl FnMut(&mut [u8]) -> bool + '_ {
        move |bytes: &mut [u8]| {
            for b in bytes.iter_mut() {
                *b = *next;
                *next = next.wrapping_add(1);
            }
            true
        }
    }

    fn apply(disk: &mut [u8], tables: &Tables) {
        for (at, bytes) in writes(tables) {
            let at = at as usize;
            disk[at..at + bytes.len()].copy_from_slice(bytes);
        }
    }

    #[test]
    fn gives_the_disk_and_each_partition_new_guids_in_both_tables() {
        let mut d = disk(&two_roots());
        let before = partitions(&mut d, BLOCK as u32, BLOCKS - 1).unwrap();
        let tables = read(&mut d, BLOCK as u32, BLOCKS - 1).unwrap().unwrap();
        let mut next = 0x10;
        let (new, renamed) = renewed(&tables, &mut counting(&mut next)).unwrap();
        apply(&mut d, &new);
        let after = partitions(&mut d, BLOCK as u32, BLOCKS - 1).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(renamed.len(), 3);
        for ((old, new), (b, a)) in renamed.iter().zip(before.iter().zip(&after)) {
            assert_eq!((b.guid, a.guid), (*old, *new));
            assert_ne!(old, new);
            assert_eq!((a.type_guid, a.first_lba), (b.type_guid, b.first_lba));
            // Version 4, variant 10.
            assert_eq!((new[7] >> 4, new[8] >> 6), (4, 2));
        }
        assert_eq!(after[1].type_guid, ROOT_TYPE);
        // Both tables are valid and the same, with the new disk GUID.
        let again = read(&mut d, BLOCK as u32, BLOCKS - 1).unwrap().unwrap();
        assert_eq!(again, new);
        assert_eq!(new.primary.header[56..72], new.backup.header[56..72]);
        assert_ne!(new.primary.header[56..72], tables.primary.header[56..72]);
        let mut broken = d.clone();
        broken[BLOCK + 40] ^= 1;
        let backup = partitions(&mut broken, BLOCK as u32, BLOCKS - 1).unwrap();
        assert_eq!(backup, after);
    }

    #[test]
    fn needs_random_bytes_and_two_matching_tables() {
        let mut d = disk(&two_roots());
        let tables = read(&mut d, BLOCK as u32, BLOCKS - 1).unwrap().unwrap();
        assert_eq!(renewed(&tables, &mut |_: &mut [u8]| false), None);
        // A backup table that does not hold the primary's entries.
        let mut other = disk(&two_roots());
        let fewer = vec![two_roots()[0]];
        let lone = disk(&fewer);
        let backup = (BLOCKS as usize - 5) * BLOCK;
        other[backup..].copy_from_slice(&lone[backup..]);
        assert_eq!(read(&mut other, BLOCK as u32, BLOCKS - 1).unwrap(), None);
        let mut broken = disk(&two_roots());
        broken[BLOCK + 40] ^= 1;
        assert_eq!(read(&mut broken, BLOCK as u32, BLOCKS - 1).unwrap(), None);
    }
}
