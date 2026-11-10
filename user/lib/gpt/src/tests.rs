extern crate std;

use super::*;
use alloc::vec;

/// The type GUID of EFI system partitions, as stored.
const ESP: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

/// A partition entry array of 4 entries of 128 bytes, with the ESP as
/// entry 1 and a nameless partition as entry 3.
fn entries() -> Vec<u8> {
    let mut entries = vec![0u8; 4 * 128];
    let mut put = |at: usize, bytes: &[u8]| entries[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, &ESP);
    put(16, &[0x11; 16]);
    put(32, &2048u64.to_le_bytes());
    put(40, &4095u64.to_le_bytes());
    for (i, unit) in "EFI system".encode_utf16().enumerate() {
        put(56 + 2 * i, &unit.to_le_bytes());
    }
    put(256, &[0x22; 16]);
    put(256 + 16, &[0x33; 16]);
    put(256 + 32, &4096u64.to_le_bytes());
    put(256 + 40, &8191u64.to_le_bytes());
    entries
}

/// A 512-byte header block at `lba`, for `entries`.
fn header_block(lba: u64, entries: &[u8]) -> Vec<u8> {
    let mut block = vec![0u8; 512];
    let mut put = |at: usize, bytes: &[u8]| block[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, b"EFI PART");
    put(8, &0x0001_0000u32.to_le_bytes());
    put(12, &92u32.to_le_bytes());
    put(24, &lba.to_le_bytes());
    put(32, &9999u64.to_le_bytes());
    put(40, &34u64.to_le_bytes());
    put(48, &9966u64.to_le_bytes());
    put(72, &2u64.to_le_bytes());
    put(80, &4u32.to_le_bytes());
    put(84, &128u32.to_le_bytes());
    put(88, &crc32(entries).to_le_bytes());
    let crc = crc32(&block[..92]);
    block[16..20].copy_from_slice(&crc.to_le_bytes());
    block
}

#[test]
fn computes_the_crc32_of_gpt() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn reads_a_valid_table() {
    let entries = entries();
    let header = header(&header_block(1, &entries), 1).expect("a valid header");
    assert_eq!(
        (header.entries_lba, header.entry_count, header.entry_size),
        (2, 4, 128)
    );
    assert_eq!(header.entries_size(), 512);
    let partitions = header.partitions(&entries).expect("a valid array");
    assert_eq!(partitions.len(), 2);
    let esp = &partitions[0];
    assert_eq!((esp.index, esp.first_lba, esp.last_lba), (1, 2048, 4095));
    assert_eq!(esp.name, "EFI system");
    assert_eq!(
        guid_string(&esp.type_guid),
        "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
    );
    assert_eq!((partitions[1].index, partitions[1].name.as_str()), (3, ""));
}

#[test]
fn refuses_tables_that_do_not_check() {
    let entries = entries();
    let block = header_block(1, &entries);
    // At another LBA than its own, with a broken signature or CRC.
    assert!(header(&block, 2).is_none());
    let mut signature = block.clone();
    signature[0] = b'X';
    assert!(header(&signature, 1).is_none());
    let mut changed = block.clone();
    changed[40] ^= 1;
    assert!(header(&changed, 1).is_none());
    // An entry array that no longer matches its CRC.
    let header = header(&block, 1).expect("a valid header");
    let mut damaged = entries.clone();
    damaged[100] ^= 1;
    assert!(header.partitions(&damaged).is_none());
    assert!(header.partitions(&entries[..256]).is_none());
}

#[test]
fn formats_guids_as_uefi_stores_them() {
    let guid = [
        0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    assert_eq!(guid_string(&guid), "00112233-4455-6677-8899-aabbccddeeff");
}
