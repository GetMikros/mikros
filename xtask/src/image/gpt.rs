//! Writing a GUID partition table.

use std::io::{self, Seek, SeekFrom, Write};
use uuid::Uuid;

pub const SECTOR: u64 = 512;
/// Partitions start on 1 MiB boundaries.
const ALIGN: u64 = (1 << 20) / SECTOR;
const ENTRIES: u64 = 128;
const ENTRY_SIZE: u64 = 128;
/// Sectors taken by the partition entry array.
const ENTRY_SECTORS: u64 = ENTRIES * ENTRY_SIZE / SECTOR;

/// A partition to write into the table.
pub struct Partition {
    pub type_guid: Uuid,
    pub guid: Uuid,
    pub name: &'static str,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// Places partitions of `sizes` bytes one after the other on 1 MiB
/// boundaries. Returns their first and last LBAs and the size of the disk in
/// sectors, including room for the backup table.
pub fn layout(sizes: &[u64]) -> (Vec<(u64, u64)>, u64) {
    let mut next = ALIGN;
    let mut places = Vec::new();
    for size in sizes {
        let sectors = size.div_ceil(SECTOR).div_ceil(ALIGN) * ALIGN;
        places.push((next, next + sectors - 1));
        next += sectors;
    }
    (places, next + ENTRY_SECTORS + 1)
}

fn entry_array(partitions: &[Partition]) -> Vec<u8> {
    let mut array = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    for (i, p) in partitions.iter().enumerate() {
        let e = &mut array[i * ENTRY_SIZE as usize..][..ENTRY_SIZE as usize];
        e[0..16].copy_from_slice(&p.type_guid.to_bytes_le());
        e[16..32].copy_from_slice(&p.guid.to_bytes_le());
        e[32..40].copy_from_slice(&p.first_lba.to_le_bytes());
        e[40..48].copy_from_slice(&p.last_lba.to_le_bytes());
        for (j, unit) in p.name.encode_utf16().take(36).enumerate() {
            e[56 + 2 * j..58 + 2 * j].copy_from_slice(&unit.to_le_bytes());
        }
    }
    array
}

fn header(
    disk_guid: Uuid,
    sectors: u64,
    (my_lba, alternate_lba, entries_lba): (u64, u64, u64),
    entries_crc: u32,
) -> [u8; SECTOR as usize] {
    let mut h = [0u8; SECTOR as usize];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&my_lba.to_le_bytes());
    h[32..40].copy_from_slice(&alternate_lba.to_le_bytes());
    h[40..48].copy_from_slice(&(2 + ENTRY_SECTORS).to_le_bytes());
    h[48..56].copy_from_slice(&(sectors - 2 - ENTRY_SECTORS).to_le_bytes());
    h[56..72].copy_from_slice(&disk_guid.to_bytes_le());
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32fast::hash(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Writes a protective MBR and the primary and backup tables for a disk of
/// `sectors` sectors.
pub fn write(
    disk: &mut (impl Write + Seek),
    sectors: u64,
    disk_guid: Uuid,
    partitions: &[Partition],
) -> io::Result<()> {
    let mut mbr = [0u8; SECTOR as usize];
    let entry = &mut mbr[446..462];
    entry[1..4].copy_from_slice(&[0, 2, 0]);
    entry[4] = 0xee;
    entry[5..8].copy_from_slice(&[0xff; 3]);
    entry[8..12].copy_from_slice(&1u32.to_le_bytes());
    let size = u32::try_from(sectors - 1).unwrap_or(u32::MAX);
    entry[12..16].copy_from_slice(&size.to_le_bytes());
    mbr[510..512].copy_from_slice(&[0x55, 0xaa]);

    let array = entry_array(partitions);
    let crc = crc32fast::hash(&array);
    let last = sectors - 1;
    let backup_entries = last - ENTRY_SECTORS;
    let writes: [(u64, &[u8]); 5] = [
        (0, &mbr),
        (1, &header(disk_guid, sectors, (1, last, 2), crc)),
        (2, &array),
        (backup_entries, &array),
        (
            last,
            &header(disk_guid, sectors, (last, 1, backup_entries), crc),
        ),
    ];
    for (lba, bytes) in writes {
        disk.seek(SeekFrom::Start(lba * SECTOR))?;
        disk.write_all(bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn u64_at(d: &[u8], o: usize) -> u64 {
        u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
    }

    fn u32_at(d: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn lays_partitions_out_on_mebibyte_boundaries() {
        let (places, sectors) = layout(&[1, 3 << 20]);
        assert_eq!(places, [(2048, 4095), (4096, 10239)]);
        assert_eq!(sectors, 10240 + 33);
    }

    #[test]
    fn writes_valid_primary_and_backup_tables() {
        let (places, sectors) = layout(&[1 << 20]);
        let type_guid = Uuid::parse_str("c12a7328-f81f-11d2-ba4b-00a0c93ec93b").unwrap();
        let part = Partition {
            type_guid,
            guid: Uuid::from_u128(7),
            name: "EFI system partition",
            first_lba: places[0].0,
            last_lba: places[0].1,
        };
        let mut disk = Cursor::new(vec![0u8; (sectors * SECTOR) as usize]);
        write(&mut disk, sectors, Uuid::from_u128(1), &[part]).unwrap();
        let d = disk.into_inner();

        assert_eq!(&d[510..512], [0x55, 0xaa]);
        assert_eq!(d[446 + 4], 0xee);
        let last = sectors - 1;
        for (lba, my, alt, entries) in [(1, 1, last, 2), (last, last, 1, last - 32)] {
            let h = &d[(lba * SECTOR) as usize..][..512];
            assert_eq!(&h[0..8], b"EFI PART");
            assert_eq!(
                (u64_at(h, 24), u64_at(h, 32), u64_at(h, 72)),
                (my, alt, entries)
            );
            let mut copy = h[..92].to_vec();
            copy[16..20].fill(0);
            assert_eq!(u32_at(h, 16), crc32fast::hash(&copy), "header CRC");
            let array = &d[(entries * SECTOR) as usize..][..16384];
            assert_eq!(u32_at(h, 88), crc32fast::hash(array), "entries CRC");
            assert_eq!(&array[0..16], type_guid.to_bytes_le());
            assert_eq!((u64_at(array, 32), u64_at(array, 40)), (2048, 4095));
            assert_eq!(&array[56..58], [b'E', 0]);
        }
        assert_eq!(
            (u64_at(&d, 512 + 40), u64_at(&d, 512 + 48)),
            (34, last - 33)
        );
    }
}
