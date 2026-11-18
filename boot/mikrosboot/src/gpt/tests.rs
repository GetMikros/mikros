use super::*;
use alloc::string::ToString;

pub(super) const BLOCK: usize = 512;
pub(super) const BLOCKS: u64 = 64;
const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

fn guid(n: u8) -> [u8; 16] {
    [n; 16]
}

fn entry(type_guid: [u8; 16], part: [u8; 16], first: u64, last: u64) -> [u8; 128] {
    let mut e = [0; 128];
    e[..16].copy_from_slice(&type_guid);
    e[16..32].copy_from_slice(&part);
    e[32..40].copy_from_slice(&first.to_le_bytes());
    e[40..48].copy_from_slice(&last.to_le_bytes());
    e
}

/// Writes a header at `lba` whose 4-block entry array is at `entries`.
fn write_table(disk: &mut [u8], lba: u64, entries: u64, parts: &[[u8; 128]]) {
    let mut array = vec![0u8; 4 * BLOCK];
    for (i, p) in parts.iter().enumerate() {
        array[i * 128..(i + 1) * 128].copy_from_slice(p);
    }
    let mut h = vec![0u8; BLOCK];
    h[..8].copy_from_slice(SIGNATURE);
    h[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&lba.to_le_bytes());
    h[72..80].copy_from_slice(&entries.to_le_bytes());
    h[80..84].copy_from_slice(&16u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h[88..92].copy_from_slice(&crc32(&array).to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    let at = entries as usize * BLOCK;
    disk[at..at + array.len()].copy_from_slice(&array);
    disk[lba as usize * BLOCK..(lba as usize + 1) * BLOCK].copy_from_slice(&h);
}

pub(super) fn disk(parts: &[[u8; 128]]) -> Vec<u8> {
    let mut d = vec![0u8; BLOCKS as usize * BLOCK];
    write_table(&mut d, 1, 2, parts);
    write_table(&mut d, BLOCKS - 1, BLOCKS - 5, parts);
    d
}

pub(super) fn two_roots() -> Vec<[u8; 128]> {
    vec![
        entry(ESP_TYPE, guid(1), 10, 19),
        entry(ROOT_TYPE, guid(2), 20, 39),
        entry(ROOT_TYPE, guid(3), 40, 49),
    ]
}

#[test]
fn reads_the_primary_table() {
    let mut d = disk(&two_roots());
    let found = partitions(&mut d, BLOCK as u32, BLOCKS - 1).unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(found[1].guid, guid(2));
    assert_eq!(
        found[1].range(BLOCK as u32, BLOCKS - 1),
        Some((20 * 512, 20 * 512))
    );
}

#[test]
fn falls_back_to_the_backup_table() {
    let mut d = disk(&two_roots());
    d[BLOCK + 40] ^= 1;
    let found = partitions(&mut d, BLOCK as u32, BLOCKS - 1).unwrap();
    assert_eq!(found.len(), 3);
    // A primary entry array that does not match its CRC32 counts as an
    // invalid primary table too.
    let mut d = disk(&two_roots());
    write_table(&mut d, 1, 2, &[entry(ROOT_TYPE, guid(9), 20, 39)]);
    d[2 * BLOCK + 16] ^= 1;
    let found = partitions(&mut d, BLOCK as u32, BLOCKS - 1).unwrap();
    assert_eq!(found[0].guid, guid(1));
    let mut d = disk(&two_roots());
    d[BLOCK] = 0;
    d[(BLOCKS as usize - 1) * BLOCK] = 0;
    assert_eq!(
        partitions(&mut d, BLOCK as u32, BLOCKS - 1),
        Err(Error::NoTable)
    );
}

#[test]
fn chooses_the_partition_the_command_line_names() {
    let parts = two_roots();
    let found = partitions(&mut disk(&parts), BLOCK as u32, BLOCKS - 1).unwrap();
    assert_eq!(
        system_partition(&found, Some(guid(3))).unwrap().guid,
        guid(3)
    );
    let missing = system_partition(&found, Some(guid(7))).unwrap_err();
    assert_eq!(
        missing,
        Error::NotOne {
            wanted: Some(guid(7)),
            candidates: vec![guid(2), guid(3)],
        }
    );
    assert_eq!(
        missing.to_string(),
        "no partition 07070707-0707-0707-0707-070707070707 on the boot disk; \
         Mikros root partitions: 02020202-0202-0202-0202-020202020202 \
         03030303-0303-0303-0303-030303030303"
    );
}

#[test]
fn lists_the_candidates_for_a_bad_guid() {
    let found = partitions(&mut disk(&two_roots()), BLOCK as u32, BLOCKS - 1).unwrap();
    let bad = Error::BadGuid {
        candidates: roots(&found),
    };
    assert_eq!(
        bad.to_string(),
        "the root=PARTUUID= option holds no valid GUID; Mikros root partitions: \
         02020202-0202-0202-0202-020202020202 03030303-0303-0303-0303-030303030303"
    );
}

#[test]
fn needs_exactly_one_root_partition_without_the_option() {
    let found = partitions(&mut disk(&two_roots()), BLOCK as u32, BLOCKS - 1).unwrap();
    let many = system_partition(&found, None).unwrap_err();
    assert!(
        many.to_string()
            .starts_with("the boot disk has more than one")
    );
    let one = &found[..2];
    assert_eq!(system_partition(one, None).unwrap().guid, guid(2));
    let none = system_partition(&found[..1], None).unwrap_err();
    assert_eq!(
        none.to_string(),
        "the boot disk has no Mikros root partition; Mikros root partitions: none"
    );
}

#[test]
fn knows_the_root_type() {
    assert_eq!(
        guid_text(&ROOT_TYPE),
        "fa730af6-8ba3-42d9-b155-466909d08135"
    );
    assert_eq!(guid_text(&ESP_TYPE), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
}

#[test]
fn checks_partition_ranges() {
    let p = |first, last| Partition {
        type_guid: ROOT_TYPE,
        guid: guid(1),
        first_lba: first,
        last_lba: last,
    };
    assert_eq!(p(5, 4).range(512, 63), None);
    assert_eq!(p(5, 64).range(512, 63), None);
    assert_eq!(p(5, 5).range(4096, 63), Some((5 * 4096, 4096)));
}
