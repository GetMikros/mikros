//! Inodes and directories.

use super::{Error, Key, ROOT_ITEM, Volume, u16_at, u32_at, u64_at};
use crate::crc::crc32c_raw;
use crate::disk::Disk;
use alloc::vec::Vec;

const INODE_ITEM: u8 = 1;
const DIR_ITEM: u8 = 84;
const DIR_INDEX: u8 = 96;
pub(super) const FS_TREE: u64 = 5;
const NODATASUM: u64 = 1 << 0;

/// An inode of a subvolume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    pub subvolume: u64,
    pub inode: u64,
}

/// The kind of an inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

/// What an inode item says about its inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inode {
    pub kind: Kind,
    pub size: u64,
    /// Whether its data has no checksums.
    pub nodatasum: bool,
}

impl<D: Disk> Volume<D> {
    /// The root directory of the top-level subvolume.
    pub fn top(&mut self) -> Result<Node, Error> {
        let top = self.subvolume(FS_TREE)?;
        Ok(Node {
            subvolume: top.id,
            inode: top.dirid,
        })
    }

    /// Whether `node` is the root directory of its subvolume.
    pub fn is_subvolume_root(&mut self, node: Node) -> Result<bool, Error> {
        Ok(self.subvolume(node.subvolume)?.dirid == node.inode)
    }

    /// The items of `kind` for `node`'s inode, in key order.
    pub(super) fn items(&mut self, node: Node, kind: u8) -> Result<Vec<(Key, Vec<u8>)>, Error> {
        let root = self.subvolume(node.subvolume)?.root;
        let (min, max) = Key::range(node.inode, kind);
        let mut found = Vec::new();
        self.search(root, min, max, &mut found)?;
        Ok(found)
    }

    pub fn inode(&mut self, node: Node) -> Result<Inode, Error> {
        let found = self.items(node, INODE_ITEM)?;
        let item = match &found[..] {
            [(_, item)] if item.len() >= 160 => item,
            _ => return Err(Error::Corrupt("an inode has no inode item")),
        };
        let kind = match u32_at(item, 52) & 0o170000 {
            0o100000 => Kind::File,
            0o040000 => Kind::Directory,
            0o120000 => Kind::Symlink,
            _ => Kind::Other,
        };
        Ok(Inode {
            kind,
            size: u64_at(item, 16),
            nodatasum: u64_at(item, 64) & NODATASUM != 0,
        })
    }

    /// Where a directory entry's location key leads from `dir`: an inode of
    /// the same subvolume, or the root of another subvolume.
    fn location(&mut self, dir: Node, location: Key) -> Result<Option<Node>, Error> {
        match location.kind {
            INODE_ITEM => Ok(Some(Node {
                subvolume: dir.subvolume,
                inode: location.objectid,
            })),
            ROOT_ITEM => {
                let sub = self.subvolume(location.objectid)?;
                Ok(Some(Node {
                    subvolume: sub.id,
                    inode: sub.dirid,
                }))
            }
            _ => Ok(None),
        }
    }

    /// The entry `name` of the directory `dir`, following an entry that
    /// leads into another subvolume to that subvolume's root.
    pub fn lookup(&mut self, dir: Node, name: &[u8]) -> Result<Option<Node>, Error> {
        let root = self.subvolume(dir.subvolume)?.root;
        let key = Key {
            objectid: dir.inode,
            kind: DIR_ITEM,
            offset: u64::from(crc32c_raw(!1, name)),
        };
        let mut found = Vec::new();
        self.search(root, key, key, &mut found)?;
        for (_, item) in found {
            for (entry_name, location) in dir_entries(&item)? {
                if entry_name == name {
                    return self.location(dir, location);
                }
            }
        }
        Ok(None)
    }

    /// The entries of the directory `dir`, as names and where they lead.
    pub fn entries(&mut self, dir: Node) -> Result<Vec<(Vec<u8>, Node)>, Error> {
        let mut entries = Vec::new();
        for (_, item) in self.items(dir, DIR_INDEX)? {
            for (name, location) in dir_entries(&item)? {
                if let Some(node) = self.location(dir, location)? {
                    entries.push((name.to_vec(), node));
                }
            }
        }
        Ok(entries)
    }
}

/// The names and location keys of the entries packed in a directory item.
fn dir_entries(item: &[u8]) -> Result<Vec<(&[u8], Key)>, Error> {
    let mut entries = Vec::new();
    let mut at = 0;
    while at < item.len() {
        let head = item
            .get(at..at + 30)
            .ok_or(Error::Corrupt("a directory item is cut short"))?;
        let data_len = usize::from(u16_at(head, 25));
        let name_len = usize::from(u16_at(head, 27));
        let name = item
            .get(at + 30..at + 30 + name_len)
            .ok_or(Error::Corrupt("a directory item is cut short"))?;
        entries.push((name, Key::at(head, 0)));
        at += 30 + name_len + data_len;
    }
    Ok(entries)
}
