//! Reading the btrfs file system of the system partition: its superblock,
//! chunk tree, root tree and subvolume trees, directories, symbolic links
//! and files, with their checksums. Nothing is ever written.

use crate::crc::crc32c;
use crate::disk::{Disk, IoError};
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

mod dir;
#[cfg(test)]
mod tests;

pub use dir::{Inode, Kind, Node};

const SUPERBLOCK_AT: u64 = 0x10000;
const SUPERBLOCK_SIZE: usize = 4096;
const MAGIC: &[u8; 8] = b"_BHRfS_M";
const CSUM_CRC32C: u16 = 0;
/// The checksum field that starts the superblock and every tree block.
const CSUM_SIZE: usize = 32;

/// The `incompat_flags` that the reader accepts.
pub mod incompat {
    pub const MIXED_BACKREF: u64 = 1 << 0;
    pub const DEFAULT_SUBVOL: u64 = 1 << 1;
    pub const COMPRESS_ZSTD: u64 = 1 << 4;
    pub const BIG_METADATA: u64 = 1 << 5;
    pub const EXTENDED_IREF: u64 = 1 << 6;
    pub const SKINNY_METADATA: u64 = 1 << 8;
    pub const NO_HOLES: u64 = 1 << 9;
    pub const ACCEPTED: u64 = MIXED_BACKREF
        | DEFAULT_SUBVOL
        | COMPRESS_ZSTD
        | BIG_METADATA
        | EXTENDED_IREF
        | SKINNY_METADATA
        | NO_HOLES;
}

/// The block group profile bits of a chunk's type. A chunk with none of
/// them is single.
const PROFILES: u64 = 0x7f8;
const DUP: u64 = 1 << 5;

const ROOT_ITEM: u8 = 132;
const CHUNK_ITEM: u8 = 228;

const FIRST_CHUNK_TREE: u64 = 256;

/// The sizes of a tree block header, a leaf item and a node's key
/// pointer.
const HEADER: usize = 101;
const ITEM: usize = 25;
const KEY_PTR: usize = 33;
const KEY: usize = 17;
const LEVELS: u8 = 8;

/// The most tree blocks kept in memory.
const CACHED: usize = 64;

/// Why the file system could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Io(IoError),
    /// The superblock's magic is wrong.
    Magic,
    /// The superblock's checksum type is not crc32c.
    ChecksumType(u16),
    /// The superblock's checksum is wrong.
    SuperblockChecksum,
    /// The file system has this many devices instead of one.
    Devices(u64),
    /// `incompat_flags` holds these flags that are not accepted.
    Incompat(u64),
    /// The chunk at this logical address has a profile other than single
    /// and DUP.
    Profile(u64),
    /// No chunk maps this logical address.
    Unmapped(u64),
    /// The tree block at this logical address has a wrong checksum.
    TreeChecksum(u64),
    /// The data block at this logical address has a wrong checksum.
    DataChecksum(u64),
    /// An extent is compressed with this algorithm, which is not zstd.
    Compression(u8),
    /// Something is not as btrfs lays it out.
    Corrupt(&'static str),
}

impl From<IoError> for Error {
    fn from(e: IoError) -> Error {
        Error::Io(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Magic => write!(f, "the partition holds no btrfs file system"),
            Error::ChecksumType(t) => write!(f, "the checksum type {t} is not crc32c"),
            Error::SuperblockChecksum => write!(f, "the superblock's checksum is wrong"),
            Error::Devices(n) => write!(f, "the file system has {n} devices, not 1"),
            Error::Incompat(flags) => {
                write!(f, "the file system uses incompatible features {flags:#x}")
            }
            Error::Profile(at) => {
                write!(f, "the chunk at {at:#x} is neither single nor DUP")
            }
            Error::Unmapped(at) => write!(f, "no chunk holds the address {at:#x}"),
            Error::TreeChecksum(at) => write!(f, "the tree block at {at:#x} has a wrong checksum"),
            Error::DataChecksum(at) => write!(f, "the data at {at:#x} has a wrong checksum"),
            Error::Compression(c) => write!(f, "an extent uses compression type {c}, not zstd"),
            Error::Corrupt(what) => write!(f, "the file system is damaged: {what}"),
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().expect("2 bytes"))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// A key of a tree item, ordered as btrfs orders them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    objectid: u64,
    kind: u8,
    offset: u64,
}

impl Key {
    fn at(b: &[u8], at: usize) -> Key {
        Key {
            objectid: u64_at(b, at),
            kind: b[at + 8],
            offset: u64_at(b, at + 9),
        }
    }

    /// The keys of `kind` for `objectid`, with every offset.
    fn range(objectid: u64, kind: u8) -> (Key, Key) {
        let key = |offset| Key {
            objectid,
            kind,
            offset,
        };
        (key(0), key(u64::MAX))
    }
}

/// The root block of a tree and its level.
#[derive(Clone, Copy, Debug)]
struct Root {
    bytenr: u64,
    level: u8,
}

/// A subvolume: its tree and the inode of its root directory.
#[derive(Clone, Copy, Debug)]
struct Subvolume {
    id: u64,
    root: Root,
    dirid: u64,
}

/// A chunk: its logical range and its type, and where its first stripe
/// starts on the device.
#[derive(Clone, Copy, Debug)]
struct Chunk {
    logical: u64,
    length: u64,
    kind: u64,
    physical: u64,
}

/// A btrfs file system on a disk.
pub struct Volume<D> {
    disk: D,
    nodesize: u32,
    chunks: Vec<Chunk>,
    root_tree: Root,
    subvolumes: Vec<Subvolume>,
    cache: Vec<(u64, Rc<[u8]>)>,
    next_slot: usize,
}

fn chunk(key: Key, item: &[u8]) -> Result<Chunk, Error> {
    if item.len() < 48 + 32 {
        return Err(Error::Corrupt("a chunk item is too short"));
    }
    let stripes = u16_at(item, 44);
    if stripes == 0 || item.len() < 48 + 32 * usize::from(stripes) {
        return Err(Error::Corrupt("a chunk item has no stripes"));
    }
    Ok(Chunk {
        logical: key.offset,
        length: u64_at(item, 0),
        kind: u64_at(item, 24),
        physical: u64_at(item, 48 + 8),
    })
}

impl<D: Disk> Volume<D> {
    /// Reads and checks the superblock and the chunk tree of the file
    /// system on `disk`.
    pub fn open(mut disk: D) -> Result<Volume<D>, Error> {
        let mut sb = vec![0u8; SUPERBLOCK_SIZE];
        disk.read(SUPERBLOCK_AT, &mut sb)?;
        if &sb[0x40..0x48] != MAGIC {
            return Err(Error::Magic);
        }
        let csum_type = u16_at(&sb, 0xc4);
        if csum_type != CSUM_CRC32C {
            return Err(Error::ChecksumType(csum_type));
        }
        if crc32c(&sb[CSUM_SIZE..]) != u32_at(&sb, 0) {
            return Err(Error::SuperblockChecksum);
        }
        let devices = u64_at(&sb, 0x88);
        if devices != 1 {
            return Err(Error::Devices(devices));
        }
        let unknown = u64_at(&sb, 0xbc) & !incompat::ACCEPTED;
        if unknown != 0 {
            return Err(Error::Incompat(unknown));
        }
        let sectorsize = u32_at(&sb, 0x90);
        let nodesize = u32_at(&sb, 0x94);
        let power = |n: u32| n.is_power_of_two() && (4096..=65536).contains(&n);
        if !power(sectorsize) || !power(nodesize) || nodesize < sectorsize {
            return Err(Error::Corrupt("the block sizes are not valid"));
        }
        let array_size = u32_at(&sb, 0xa0) as usize;
        let array = sb
            .get(0x32b..0x32b + array_size)
            .ok_or(Error::Corrupt("the system chunk array is too long"))?;
        let mut chunks = Vec::new();
        let mut at = 0;
        while at < array.len() {
            let item = array
                .get(at..)
                .filter(|rest| rest.len() >= KEY + 48)
                .ok_or(Error::Corrupt("the system chunk array is cut short"))?;
            let key = Key::at(item, 0);
            let stripes = usize::from(u16_at(item, KEY + 44));
            let len = 48 + 32 * stripes;
            let body = item
                .get(KEY..KEY + len)
                .ok_or(Error::Corrupt("the system chunk array is cut short"))?;
            if key.kind != CHUNK_ITEM {
                return Err(Error::Corrupt("the system chunk array holds another item"));
            }
            chunks.push(chunk(key, body)?);
            at += KEY + len;
        }
        let mut volume = Volume {
            disk,
            nodesize,
            chunks,
            root_tree: Root {
                bytenr: u64_at(&sb, 0x50),
                level: sb[0xc6],
            },
            subvolumes: Vec::new(),
            cache: Vec::new(),
            next_slot: 0,
        };
        let chunk_root = Root {
            bytenr: u64_at(&sb, 0x58),
            level: sb[0xc7],
        };
        let (min, max) = Key::range(FIRST_CHUNK_TREE, CHUNK_ITEM);
        let mut found = Vec::new();
        volume.search(chunk_root, min, max, &mut found)?;
        for (key, item) in found {
            let c = chunk(key, &item)?;
            if !volume.chunks.iter().any(|known| known.logical == c.logical) {
                volume.chunks.push(c);
            }
        }
        Ok(volume)
    }

    /// The physical address of `len` bytes at `logical`.
    fn map(&self, logical: u64, len: u64) -> Result<u64, Error> {
        let c = self
            .chunks
            .iter()
            .find(|c| logical >= c.logical && logical - c.logical < c.length)
            .ok_or(Error::Unmapped(logical))?;
        if c.kind & PROFILES & !DUP != 0 {
            return Err(Error::Profile(c.logical));
        }
        if len > c.length - (logical - c.logical) {
            return Err(Error::Corrupt("a read crosses the end of a chunk"));
        }
        Ok(c.physical + (logical - c.logical))
    }

    fn read_logical(&mut self, logical: u64, buf: &mut [u8]) -> Result<(), Error> {
        let physical = self.map(logical, buf.len() as u64)?;
        Ok(self.disk.read(physical, buf)?)
    }

    /// The tree block at `bytenr`, checked to be of `level`.
    fn block(&mut self, bytenr: u64, level: u8) -> Result<Rc<[u8]>, Error> {
        let cached = self.cache.iter().find(|(at, _)| *at == bytenr);
        let block = match cached {
            Some((_, block)) => block.clone(),
            None => {
                let mut bytes = vec![0u8; self.nodesize as usize];
                self.read_logical(bytenr, &mut bytes)?;
                if crc32c(&bytes[CSUM_SIZE..]) != u32_at(&bytes, 0) {
                    return Err(Error::TreeChecksum(bytenr));
                }
                let block: Rc<[u8]> = bytes.into();
                if self.cache.len() < CACHED {
                    self.cache.push((bytenr, block.clone()));
                } else {
                    self.cache[self.next_slot] = (bytenr, block.clone());
                    self.next_slot = (self.next_slot + 1) % CACHED;
                }
                block
            }
        };
        if u64_at(&block, 48) != bytenr || block[100] != level {
            return Err(Error::Corrupt("a tree block is not where its parent says"));
        }
        Ok(block)
    }

    /// Appends the items of the tree at `root` whose keys lie from `min` to
    /// `max` to `out`, in key order.
    fn search(
        &mut self,
        root: Root,
        min: Key,
        max: Key,
        out: &mut Vec<(Key, Vec<u8>)>,
    ) -> Result<(), Error> {
        if root.level >= LEVELS {
            return Err(Error::Corrupt("a tree is too deep"));
        }
        let block = self.block(root.bytenr, root.level)?;
        let count = u32_at(&block, 96) as usize;
        let size = if root.level == 0 { ITEM } else { KEY_PTR };
        if count > (block.len() - HEADER) / size {
            return Err(Error::Corrupt("a tree block holds too many items"));
        }
        let entry = |i: usize| HEADER + i * size;
        if root.level == 0 {
            for i in 0..count {
                let key = Key::at(&block, entry(i));
                if key < min || key > max {
                    continue;
                }
                let offset = u32_at(&block, entry(i) + 17) as usize;
                let len = u32_at(&block, entry(i) + 21) as usize;
                let data = block
                    .get(HEADER + offset..HEADER + offset + len)
                    .ok_or(Error::Corrupt("an item lies outside its leaf"))?;
                out.push((key, data.to_vec()));
            }
            return Ok(());
        }
        for i in 0..count {
            let key = Key::at(&block, entry(i));
            if key > max {
                break;
            }
            // A child holds the keys from its own up to the next child's.
            if i + 1 < count && Key::at(&block, entry(i + 1)) <= min {
                continue;
            }
            let child = Root {
                bytenr: u64_at(&block, entry(i) + KEY),
                level: root.level - 1,
            };
            self.search(child, min, max, out)?;
        }
        Ok(())
    }

    /// The tree of `id` and its root directory's inode, from its root item
    /// in the root tree.
    fn root_item(&mut self, id: u64) -> Result<Option<(Root, u64)>, Error> {
        let (min, max) = Key::range(id, ROOT_ITEM);
        let mut found = Vec::new();
        self.search(self.root_tree, min, max, &mut found)?;
        let Some((_, item)) = found.pop() else {
            return Ok(None);
        };
        if item.len() < 239 {
            return Err(Error::Corrupt("a root item is too short"));
        }
        let root = Root {
            bytenr: u64_at(&item, 176),
            level: item[238],
        };
        Ok(Some((root, u64_at(&item, 168))))
    }

    fn subvolume(&mut self, id: u64) -> Result<Subvolume, Error> {
        if let Some(known) = self.subvolumes.iter().find(|s| s.id == id) {
            return Ok(*known);
        }
        let (root, dirid) = self
            .root_item(id)?
            .ok_or(Error::Corrupt("a subvolume has no root item"))?;
        let subvolume = Subvolume { id, root, dirid };
        self.subvolumes.push(subvolume);
        Ok(subvolume)
    }
}
