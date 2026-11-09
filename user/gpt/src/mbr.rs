//! Reading master boot records, and the extended boot records that an
//! extended partition chains: the primary partitions in entries 1 to 4,
//! and the logical partitions, numbered from 5 in the order of the chain.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The type of the protective entry of a GPT disk's MBR.
const PROTECTIVE: u8 = 0xEE;
/// The types of extended partitions.
const EXTENDED: [u8; 3] = [0x05, 0x0F, 0x85];
/// The hidden types, whose partitions are not mounted automatically.
const HIDDEN: [u8; 6] = [0x11, 0x14, 0x16, 0x1B, 0x1C, 0x1E];
/// Where a boot record holds the disk signature and its four entries.
const SIGNATURE_AT: usize = 440;
const ENTRIES_AT: usize = 446;
/// The bytes a boot record ends in.
const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xAA];

/// An entry of a boot record: its type, and its first block and block
/// count, from the base that the record's kind gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    kind: u8,
    start: u32,
    count: u32,
}

/// Entry `i`, 0 to 3, of the boot record `sector`, at least 512 bytes.
fn entry(sector: &[u8], i: usize) -> Entry {
    let at = ENTRIES_AT + 16 * i;
    let u32_at = |at: usize| u32::from_le_bytes(sector[at..at + 4].try_into().expect("4 bytes"));
    Entry {
        kind: sector[at + 4],
        start: u32_at(at + 8),
        count: u32_at(at + 12),
    }
}

fn is_boot_record(sector: &[u8]) -> bool {
    sector.len() >= 512 && sector[510..512] == BOOT_SIGNATURE
}

/// Whether `kind` is the type of an extended partition.
pub fn is_extended(kind: u8) -> bool {
    EXTENDED.contains(&kind)
}

/// Whether `kind` is one of the hidden types.
pub fn is_hidden(kind: u8) -> bool {
    HIDDEN.contains(&kind)
}

/// Whether `sector`, the first block of a disk, is a master boot record
/// other than a GPT disk's protective one: it ends in `0x55 0xAA`, and no
/// entry has the type `0xEE`.
pub fn is_mbr(sector: &[u8]) -> bool {
    is_boot_record(sector) && (0..4).all(|i| entry(sector, i).kind != PROTECTIVE)
}

/// The disk signature of the master boot record `sector`.
pub fn signature(sector: &[u8]) -> u32 {
    u32::from_le_bytes(
        sector[SIGNATURE_AT..SIGNATURE_AT + 4]
            .try_into()
            .expect("4 bytes"),
    )
}

/// A partition of an MBR disk: its number, its type, and its first and
/// last LBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    pub number: u32,
    pub kind: u8,
    pub first_lba: u64,
    pub last_lba: u64,
}

impl Partition {
    /// The identifier of the partition on the disk of `signature`: the
    /// signature as eight lowercase hexadecimal digits, a hyphen, and the
    /// number as two.
    pub fn identifier(&self, signature: u32) -> String {
        format!("{signature:08x}-{:02x}", self.number)
    }

    fn new(number: u32, kind: u8, first: u64, count: u32) -> Option<Partition> {
        let last = first.checked_add(u64::from(count))?.checked_sub(1)?;
        (count > 0).then_some(Partition {
            number,
            kind,
            first_lba: first,
            last_lba: last,
        })
    }
}

/// The primary partitions of the master boot record `sector`: the entries
/// 1 to 4 that have blocks and a non-zero type other than an extended
/// one, numbered by their entry. Also the first LBA of the first extended
/// partition, when there is one.
pub fn primaries(sector: &[u8]) -> (Vec<Partition>, Option<u64>) {
    let mut partitions = Vec::new();
    let mut extended = None;
    for i in 0..4 {
        let e = entry(sector, i);
        if e.kind == 0 {
            continue;
        }
        if is_extended(e.kind) {
            extended = extended.or((e.count > 0).then_some(u64::from(e.start)));
            continue;
        }
        let number = i as u32 + 1;
        partitions.extend(Partition::new(number, e.kind, u64::from(e.start), e.count));
    }
    (partitions, extended)
}

/// What an extended boot record holds: its logical partition, as the
/// type, first LBA and block count, when it has one, and the LBA of the
/// next record, when there is one.
struct Record {
    partition: Option<(u8, u64, u32)>,
    next: Option<u64>,
}

/// What the extended boot record `sector`, read from `lba` in the
/// extended partition that starts at `extended`, holds, or `None` when
/// `sector` is not a boot record.
fn logical(sector: &[u8], lba: u64, extended: u64) -> Option<Record> {
    if !is_boot_record(sector) {
        return None;
    }
    let first = entry(sector, 0);
    let partition = (first.kind != 0 && first.count > 0)
        .then(|| (first.kind, lba + u64::from(first.start), first.count));
    let link = entry(sector, 1);
    let next = (is_extended(link.kind) && link.count > 0).then(|| extended + u64::from(link.start));
    Some(Record { partition, next })
}

/// A walk along the chain of extended boot records of an extended
/// partition, which collects its logical partitions, numbered from 5 in
/// the order of the chain. The chain ends at a record without a next
/// one, at one that is not a boot record or could not be read, at one
/// that the walk met already, and after `limit` records.
pub struct Chain {
    extended: u64,
    limit: usize,
    at: Option<u64>,
    seen: Vec<u64>,
    partitions: Vec<Partition>,
}

impl Chain {
    /// A walk of the chain of the extended partition that starts at
    /// `extended`.
    pub fn new(extended: u64, limit: usize) -> Chain {
        Chain {
            extended,
            limit,
            at: Some(extended),
            seen: Vec::new(),
            partitions: Vec::new(),
        }
    }

    /// The LBA of the next record to read, or `None` once the chain has
    /// ended.
    pub fn next(&self) -> Option<u64> {
        self.at
            .filter(|at| self.seen.len() < self.limit && !self.seen.contains(at))
    }

    /// Takes the record read from the LBA that `next` gave, `None` when
    /// it could not be read.
    pub fn take(&mut self, sector: Option<&[u8]>) {
        let Some(lba) = self.next() else {
            return;
        };
        self.seen.push(lba);
        let Some(record) = sector.and_then(|s| logical(s, lba, self.extended)) else {
            self.at = None;
            return;
        };
        if let Some((kind, first, count)) = record.partition {
            let number = 5 + self.partitions.len() as u32;
            self.partitions
                .extend(Partition::new(number, kind, first, count));
        }
        self.at = record.next;
    }

    /// The logical partitions found.
    pub fn partitions(self) -> Vec<Partition> {
        self.partitions
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec;

    /// A boot record with `entries` as (type, start, count), and the disk
    /// signature `0x1234abcd`.
    fn record(entries: &[(u8, u32, u32)]) -> Vec<u8> {
        let mut sector = vec![0u8; 512];
        sector[440..444].copy_from_slice(&0x1234_abcdu32.to_le_bytes());
        for (i, &(kind, start, count)) in entries.iter().enumerate() {
            let at = 446 + 16 * i;
            sector[at + 4] = kind;
            sector[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
            sector[at + 12..at + 16].copy_from_slice(&count.to_le_bytes());
        }
        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn reads_primaries_and_the_extended_partition() {
        let sector = record(&[
            (0x0c, 2048, 1000),
            (0, 0, 0),
            (0x0f, 10_000, 5000),
            (0x1c, 4000, 10),
        ]);
        assert!(is_mbr(&sector));
        assert_eq!(signature(&sector), 0x1234_abcd);
        let (partitions, extended) = primaries(&sector);
        assert_eq!(extended, Some(10_000));
        assert_eq!(
            partitions,
            [
                Partition {
                    number: 1,
                    kind: 0x0c,
                    first_lba: 2048,
                    last_lba: 3047
                },
                Partition {
                    number: 4,
                    kind: 0x1c,
                    first_lba: 4000,
                    last_lba: 4009
                },
            ]
        );
        assert!(is_hidden(0x1c) && !is_hidden(0x0c));
        assert_eq!(partitions[1].identifier(0x1234_abcd), "1234abcd-04");
    }

    #[test]
    fn refuses_protective_and_unsigned_records() {
        assert!(!is_mbr(&record(&[(0xee, 1, 100)])));
        let mut sector = record(&[(0x0c, 2048, 1000)]);
        sector[511] = 0;
        assert!(!is_mbr(&sector));
        assert!(!is_mbr(&sector[..200]));
        // An entry without blocks is no partition.
        assert!(primaries(&record(&[(0x83, 2048, 0)])).0.is_empty());
    }

    #[test]
    fn walks_the_chain_of_logical_partitions() {
        // Records at 10000, 12000 and 15000; the second has no partition.
        let records = [
            (10_000, record(&[(0x83, 63, 100), (0x05, 2000, 3000)])),
            (12_000, record(&[(0, 0, 0), (0x05, 5000, 100)])),
            (15_000, record(&[(0x07, 63, 50)])),
        ];
        let mut chain = Chain::new(10_000, 128);
        while let Some(lba) = chain.next() {
            let sector = records.iter().find(|(at, _)| *at == lba).map(|(_, s)| s);
            chain.take(sector.map(|s| s.as_slice()));
        }
        assert_eq!(
            chain.partitions(),
            [
                Partition {
                    number: 5,
                    kind: 0x83,
                    first_lba: 10_063,
                    last_lba: 10_162
                },
                Partition {
                    number: 6,
                    kind: 0x07,
                    first_lba: 15_063,
                    last_lba: 15_112
                },
            ]
        );
    }

    #[test]
    fn ends_chains_that_loop_or_break() {
        // A record that links back to itself.
        let looping = record(&[(0x83, 63, 100), (0x05, 0, 3000)]);
        let mut chain = Chain::new(10_000, 128);
        let mut reads = 0;
        while chain.next().is_some() {
            reads += 1;
            chain.take(Some(&looping));
        }
        assert_eq!((reads, chain.partitions().len()), (1, 1));
        // A record that cannot be read ends the chain.
        let mut chain = Chain::new(10_000, 128);
        chain.take(None);
        assert_eq!(chain.next(), None);
        // The limit holds even for a chain that never repeats.
        let mut chain = Chain::new(0, 3);
        let mut at = 0;
        while let Some(lba) = chain.next() {
            at += 1;
            chain.take(Some(&record(&[(0x83, 1, 1), (0x05, at, 10)])));
            assert!(lba < 10);
        }
        assert_eq!(chain.partitions().len(), 3);
    }
}
