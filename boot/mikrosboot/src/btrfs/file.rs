//! Reading the contents of files and symbolic links.

use super::zstd::decompress;
use super::{Error, Inode, Key, Kind, Node, Volume, u16_at, u32_at, u64_at};
use crate::crc::crc32c;
use crate::disk::Disk;
use alloc::vec;
use alloc::vec::Vec;

pub(super) const EXTENT_DATA: u8 = 108;
const EXTENT_CSUM: u8 = 128;
const EXTENT_CSUM_OBJECTID: u64 = -10i64 as u64;
const COMPRESS_NONE: u8 = 0;
/// The most bytes that a compressed extent holds once decompressed.
pub(super) const COMPRESSED_MAX: u64 = 128 * 1024;
/// The most bytes read from an uncompressed extent at a time.
const READ_STEP: u64 = 1 << 20;
/// The longest symbolic link target.
const LINK_MAX: u64 = 4095;

impl<D: Disk> Volume<D> {
    /// The target of the symbolic link `node`.
    pub fn read_link(&mut self, node: Node) -> Result<Vec<u8>, Error> {
        let inode = self.inode(node)?;
        if inode.kind != Kind::Symlink || inode.size > LINK_MAX {
            return Err(Error::Corrupt("a symbolic link is not valid"));
        }
        let mut target = vec![0; inode.size as usize];
        self.read(node, &mut target)?;
        Ok(target)
    }

    /// Fills `out` with the contents of the file `node`, whose size `out`
    /// must have: zeros where it has holes or preallocated extents.
    pub fn read(&mut self, node: Node, out: &mut [u8]) -> Result<(), Error> {
        let inode = self.inode(node)?;
        if out.len() as u64 != inode.size {
            return Err(Error::Corrupt("a file changed its size"));
        }
        out.fill(0);
        for (key, item) in self.items(node, EXTENT_DATA)? {
            if key.offset >= inode.size {
                continue;
            }
            let dest = &mut out[key.offset as usize..];
            self.extent(&item, inode, dest)?;
        }
        Ok(())
    }

    /// Copies the data of the file extent `item` to the start of `dest`, at
    /// most `dest.len()` bytes.
    fn extent(&mut self, item: &[u8], inode: Inode, dest: &mut [u8]) -> Result<(), Error> {
        if item.len() < 21 {
            return Err(Error::Corrupt("a file extent item is too short"));
        }
        let ram_bytes = u64_at(item, 8);
        let compression = item[16];
        if item[17] != 0 || u16_at(item, 18) != 0 {
            return Err(Error::Corrupt("a file extent is encrypted or encoded"));
        }
        let kind = item[20];
        if kind == 0 {
            let data = &item[21..];
            let data = match compression {
                COMPRESS_NONE => data.to_vec(),
                _ => decompress(compression, data, ram_bytes)?,
            };
            let len = data.len().min(dest.len());
            dest[..len].copy_from_slice(&data[..len]);
            return Ok(());
        }
        if item.len() < 53 {
            return Err(Error::Corrupt("a file extent item is too short"));
        }
        let disk_bytenr = u64_at(item, 21);
        let disk_bytes = u64_at(item, 29);
        let offset = u64_at(item, 37);
        let bytes = u64_at(item, 45);
        // Preallocated extents and holes read as zeros, which `dest` holds.
        if kind == 2 || disk_bytenr == 0 {
            return Ok(());
        }
        if kind != 1 {
            return Err(Error::Corrupt("a file extent has an unknown type"));
        }
        let len = bytes.min(dest.len() as u64) as usize;
        let checked = !inode.nodatasum;
        if compression != COMPRESS_NONE {
            if disk_bytes > COMPRESSED_MAX {
                return Err(Error::Corrupt("a compressed extent is too large"));
            }
            let mut raw = vec![0; disk_bytes as usize];
            self.read_data(disk_bytenr, &mut raw, checked)?;
            let data = decompress(compression, &raw, ram_bytes)?;
            let source = data
                .get(offset as usize..offset as usize + len)
                .ok_or(Error::Corrupt("a compressed extent is too short"))?;
            dest[..len].copy_from_slice(source);
            return Ok(());
        }
        if offset.checked_add(bytes).is_none_or(|end| end > disk_bytes) {
            return Err(Error::Corrupt("a file extent lies outside its disk extent"));
        }
        // Whole sectors are read so that their checksums can be checked.
        let sector = u64::from(self.sectorsize);
        let start = offset / sector * sector;
        let end = (offset + len as u64)
            .next_multiple_of(sector)
            .min(disk_bytes);
        let mut done = 0;
        let mut at = start;
        while at < end {
            let step = (end - at).min(READ_STEP);
            let mut buf = vec![0; step as usize];
            self.read_data(disk_bytenr + at, &mut buf, checked)?;
            let from = (offset + done as u64).saturating_sub(at) as usize;
            let take = (buf.len() - from).min(len - done);
            dest[done..done + take].copy_from_slice(&buf[from..from + take]);
            done += take;
            at += step;
        }
        Ok(())
    }

    /// Fills `buf` with the data at `logical`, checking the checksum of each
    /// sector that has one when `checked`.
    fn read_data(&mut self, logical: u64, buf: &mut [u8], checked: bool) -> Result<(), Error> {
        self.read_logical(logical, buf)?;
        let Some(tree) = self.csum_tree.filter(|_| checked) else {
            return Ok(());
        };
        let sector = u64::from(self.sectorsize);
        // One item holds a checksum for each of at most this many bytes.
        let span = u64::from(self.nodesize) / 4 * sector;
        let end = logical + buf.len() as u64;
        let key = |offset| Key {
            objectid: EXTENT_CSUM_OBJECTID,
            kind: EXTENT_CSUM,
            offset,
        };
        let mut sums = Vec::new();
        self.search(
            tree,
            key(logical.saturating_sub(span)),
            key(end - 1),
            &mut sums,
        )?;
        for (i, data) in buf.chunks(sector as usize).enumerate() {
            let at = logical + i as u64 * sector;
            let sum = sums.iter().find_map(|(k, item)| {
                let index = at.checked_sub(k.offset)? / sector;
                item.get(index as usize * 4..index as usize * 4 + 4)
            });
            if let Some(sum) = sum
                && crc32c(data) != u32_at(sum, 0)
            {
                return Err(Error::DataChecksum(at));
            }
        }
        Ok(())
    }
}
