//! Listing directories with `getdents64`.
//!
//! A directory descriptor reads its listing in the batches of
//! `ReadDirectory` and hands out each entry once. A folder above
//! namespace entries holds its whole listing from the start. Its position, which
//! `d_off` and `lseek` use, is the number of entries handed out, since
//! the listing has a stable order; seeking elsewhere lists again from the
//! start and skips that many.

use crate::errno::{self, EINVAL, EIO, ENOTDIR};
use crate::fd::{self, Description};
use crate::io::{SEEK_CUR, SEEK_SET};
use crate::lock::Lock;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::fs::{DirEntry, NodeType};

const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;
const DT_SOCK: u8 = 12;
/// The bytes of `struct dirent` before `d_name`.
const HEADER: usize = 19;
const FOREVER: i64 = i64::MAX;

/// The state of a directory listing.
#[derive(Default)]
pub struct Listing {
    /// The batch being handed out, and how much of it is.
    batch: Vec<DirEntry>,
    used: usize,
    /// The cookie of the batch after it.
    next: u64,
    /// Whether the listing has no more batches.
    ended: bool,
    /// The number of entries handed out or skipped.
    position: u64,
    /// The position that a seek asked for, until it is reached.
    target: u64,
    /// Whether `batch` is the whole listing, which a seek back keeps.
    held: bool,
}

/// A folder above namespace entries that no entry covers: its path, and
/// a listing of the next component of each entry below it, as folders.
pub struct Folder {
    pub path: String,
    pub listing: Lock<Listing>,
}

impl Folder {
    pub fn new(path: String, names: Vec<String>) -> Folder {
        let batch = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| DirEntry {
                name,
                r#type: NodeType::Directory,
                inode: i as u64 + 1,
                attributes: None,
            })
            .collect();
        let listing = Listing {
            batch,
            ended: true,
            held: true,
            ..Listing::default()
        };
        Folder {
            path,
            listing: Lock::new(listing),
        }
    }
}

fn dirent_type(kind: NodeType) -> u8 {
    match kind {
        NodeType::File => DT_REG,
        NodeType::Directory => DT_DIR,
        NodeType::Symlink => DT_LNK,
        NodeType::Socket => DT_SOCK,
        NodeType::Unknown(_) => DT_UNKNOWN,
    }
}

/// `getdents64`: fills the `len` bytes at `buf` with as many entries as
/// fit, and returns how many bytes it filled, which is 0 at the end.
pub fn getdents(fd: i64, buf: *mut u8, len: u64) -> i64 {
    let Some(description) = fd::get(fd) else {
        return -errno::EBADF;
    };
    let (node, listing) = match &*description {
        Description::Directory(node, listing) => (Some(node), listing),
        Description::Folder(folder) => (None, &folder.listing),
        _ => return -ENOTDIR,
    };
    let mut guard = listing.lock();
    let l = &mut *guard;
    let mut filled = 0;
    loop {
        if l.used == l.batch.len() {
            let Some(node) = node.filter(|_| !l.ended) else {
                break;
            };
            let read = node.node.lock().read_directory(l.next, false, FOREVER);
            let error = match read {
                Ok(Ok(r)) => {
                    l.ended = r.entries.is_empty();
                    l.batch = r.entries;
                    l.used = 0;
                    l.next = r.next;
                    continue;
                }
                Ok(Err(e)) => errno::from_fs(e),
                Err(_) => EIO,
            };
            return if filled > 0 { filled as i64 } else { -error };
        }
        if l.position < l.target {
            l.used += 1;
            l.position += 1;
            continue;
        }
        let entry = &l.batch[l.used];
        let name = entry.name.as_bytes();
        let size = (HEADER + name.len() + 1).next_multiple_of(8);
        if filled + size > len as usize {
            if filled == 0 {
                return -EINVAL;
            }
            break;
        }
        let (inode, kind) = (entry.inode, dirent_type(entry.r#type));
        l.used += 1;
        l.position += 1;
        l.target = l.position;
        // SAFETY: musl passes `len` bytes, and the record fits in them.
        unsafe {
            let at = buf.add(filled);
            at.cast::<u64>().write_unaligned(inode);
            at.add(8).cast::<u64>().write_unaligned(l.position);
            at.add(16).cast::<u16>().write_unaligned(size as u16);
            at.add(18).write(kind);
            core::ptr::copy_nonoverlapping(name.as_ptr(), at.add(HEADER), name.len());
            core::ptr::write_bytes(at.add(HEADER + name.len()), 0, size - HEADER - name.len());
        }
        filled += size;
    }
    filled as i64
}

/// `lseek` on a directory: to a position that `d_off` gave, or the
/// current one with `SEEK_CUR` and 0.
pub fn seek(listing: &Lock<Listing>, offset: i64, whence: i64) -> i64 {
    let mut l = listing.lock();
    match whence {
        SEEK_CUR if offset == 0 => l.target as i64,
        SEEK_SET if offset >= 0 => {
            let offset = offset as u64;
            if offset < l.position && l.held {
                l.used = 0;
                l.position = 0;
            } else if offset < l.position {
                *l = Listing::default();
            }
            l.target = offset;
            offset as i64
        }
        _ => -EINVAL,
    }
}
