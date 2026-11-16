//! The owners of the nodes of a btrfs image (MIKSPEC-0023.R46): mkfs.btrfs
//! `--rootdir` keeps those of the build's files, so every inode item of
//! every subvolume is given owner and group 0 in the finished image,
//! where the tree blocks lie. Each block that changes gets its checksum
//! again, in each of its copies. The image must be on one device, with
//! crc32c checksums.

use anyhow::{Context, Result, bail, ensure};
use mikrosboot::crc::crc32c;
use std::collections::BTreeSet;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

const SUPERBLOCK_AT: u64 = 0x10000;
const SUPERBLOCK_SIZE: usize = 4096;
const MAGIC: &[u8; 8] = b"_BHRfS_M";
const CSUM_CRC32C: u16 = 0;
const CSUM_SIZE: usize = 32;
/// The sizes of a tree block's header, a leaf item and a node's key
/// pointer, and of a key.
const HEADER: usize = 101;
const ITEM: usize = 25;
const KEY_PTR: usize = 33;
const INODE_ITEM: u8 = 1;
const ROOT_ITEM: u8 = 132;
const CHUNK_ITEM: u8 = 228;
const FS_TREE: u64 = 5;
const FIRST_FREE: u64 = 256;
const LAST_FREE: u64 = u64::MAX - 256;
/// Where an inode item holds the owner and the group, and a root item
/// the address of its tree's root.
const UID_AT: usize = 44;
const GID_AT: usize = 48;
const ROOT_BYTENR_AT: usize = 176;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// A chunk: the logical range it maps, and where each copy of it lies.
struct Chunk {
    logical: u64,
    length: u64,
    copies: Vec<u64>,
}

/// The chunk that the chunk item `item` describes, which maps from
/// `logical`.
fn chunk(logical: u64, item: &[u8]) -> Result<Chunk> {
    ensure!(item.len() >= 48, "a chunk item is too short");
    let stripes = usize::from(u16_at(item, 44));
    ensure!(
        stripes > 0 && item.len() >= 48 + 32 * stripes,
        "a chunk item has no stripes"
    );
    let mut copies = Vec::with_capacity(stripes);
    for i in 0..stripes {
        let stripe = &item[48 + 32 * i..];
        ensure!(
            u64_at(stripe, 0) == 1,
            "the image is on more than one device"
        );
        copies.push(u64_at(stripe, 8));
    }
    Ok(Chunk {
        logical,
        length: u64_at(item, 0),
        copies,
    })
}

struct Image {
    file: File,
    nodesize: usize,
    chunks: Vec<Chunk>,
}

impl Image {
    /// Where the copies of the `len` bytes at `logical` lie.
    fn map(&self, logical: u64, len: u64) -> Result<Vec<u64>> {
        let chunk = self
            .chunks
            .iter()
            .find(|c| logical >= c.logical && logical + len <= c.logical + c.length)
            .with_context(|| format!("no chunk maps {logical:#x}"))?;
        Ok(chunk
            .copies
            .iter()
            .map(|at| at + (logical - chunk.logical))
            .collect())
    }

    /// The tree block at `logical`, from its first copy, checked.
    fn block(&self, logical: u64) -> Result<Vec<u8>> {
        let at = self.map(logical, self.nodesize as u64)?[0];
        let mut block = vec![0u8; self.nodesize];
        self.file.read_exact_at(&mut block, at)?;
        ensure!(
            u32_at(&block, 0) == crc32c(&block[CSUM_SIZE..]),
            "the tree block at {logical:#x} has a wrong checksum"
        );
        ensure!(
            u64_at(&block, 0x30) == logical,
            "the tree block at {logical:#x} names another address"
        );
        Ok(block)
    }

    /// Writes `block` to every copy of `logical`, with its checksum.
    fn write(&self, logical: u64, block: &mut [u8]) -> Result<()> {
        let sum = crc32c(&block[CSUM_SIZE..]);
        block[..CSUM_SIZE].fill(0);
        block[..4].copy_from_slice(&sum.to_le_bytes());
        for at in self.map(logical, block.len() as u64)? {
            self.file.write_all_at(block, at)?;
        }
        Ok(())
    }

    /// Calls `leaf` with each leaf of the tree whose root is at `root`,
    /// once, and writes back those it changes.
    fn walk(
        &self,
        root: u64,
        seen: &mut BTreeSet<u64>,
        leaf: &mut dyn FnMut(&mut [u8]) -> Result<bool>,
    ) -> Result<()> {
        let mut todo = vec![root];
        while let Some(logical) = todo.pop() {
            if !seen.insert(logical) {
                continue;
            }
            let mut block = self.block(logical)?;
            let items = u32_at(&block, 0x60) as usize;
            if block[0x64] == 0 {
                if leaf(&mut block)? {
                    self.write(logical, &mut block)?;
                }
                continue;
            }
            for i in 0..items {
                todo.push(u64_at(&block, HEADER + KEY_PTR * i + 17));
            }
        }
        Ok(())
    }
}

/// The items of `leaf`: their key's object ID and type, and where their
/// data lies.
fn items(leaf: &[u8]) -> Vec<(u64, u8, usize, usize)> {
    (0..u32_at(leaf, 0x60) as usize)
        .map(|i| {
            let item = HEADER + ITEM * i;
            let offset = HEADER + u32_at(leaf, item + 17) as usize;
            let size = u32_at(leaf, item + 21) as usize;
            (u64_at(leaf, item), leaf[item + 8], offset, size)
        })
        .collect()
}

/// Gives every node of the btrfs in `path` owner and group 0.
pub fn make_owned_by_0(path: &Path) -> Result<()> {
    let file = File::options()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut sb = vec![0u8; SUPERBLOCK_SIZE];
    file.read_exact_at(&mut sb, SUPERBLOCK_AT)?;
    ensure!(
        &sb[0x40..0x48] == MAGIC,
        "{} holds no btrfs",
        path.display()
    );
    ensure!(
        u16_at(&sb, 0xc4) == CSUM_CRC32C,
        "the image's checksums are not crc32c"
    );
    ensure!(
        u64_at(&sb, 0x88) == 1,
        "the image is on more than one device"
    );
    let nodesize = u32_at(&sb, 0x94) as usize;
    // The system chunks, which the superblock holds, map the chunk tree.
    let array = &sb[0x32b..0x32b + u32_at(&sb, 0xa0) as usize];
    let mut chunks = Vec::new();
    let mut at = 0;
    while at + 17 <= array.len() {
        let (logical, kind) = (u64_at(array, at + 9), array[at + 8]);
        if kind != CHUNK_ITEM {
            bail!("the superblock's chunk array holds an item of type {kind}");
        }
        let stripes = usize::from(u16_at(array, at + 17 + 44));
        let len = 48 + 32 * stripes;
        chunks.push(chunk(logical, &array[at + 17..at + 17 + len])?);
        at += 17 + len;
    }
    let mut image = Image {
        file,
        nodesize,
        chunks,
    };
    // The chunk tree's leaves hold every chunk, with the logical address
    // each maps from as its key's offset.
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    image.walk(u64_at(&sb, 0x58), &mut seen, &mut |leaf| {
        for i in 0..u32_at(leaf, 0x60) as usize {
            let item = HEADER + ITEM * i;
            if leaf[item + 8] == CHUNK_ITEM {
                let offset = HEADER + u32_at(leaf, item + 17) as usize;
                let size = u32_at(leaf, item + 21) as usize;
                all.push(chunk(u64_at(leaf, item + 9), &leaf[offset..offset + size])?);
            }
        }
        Ok(false)
    })?;
    image.chunks = all;
    // The root tree names the trees of the subvolumes.
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();
    image.walk(u64_at(&sb, 0x50), &mut seen, &mut |leaf| {
        for (id, kind, offset, size) in items(leaf) {
            let subvolume = id == FS_TREE || (FIRST_FREE..=LAST_FREE).contains(&id);
            if kind == ROOT_ITEM && subvolume && size >= ROOT_BYTENR_AT + 8 {
                roots.push(u64_at(leaf, offset + ROOT_BYTENR_AT));
            }
        }
        Ok(false)
    })?;
    let mut seen = BTreeSet::new();
    for root in roots {
        image.walk(root, &mut seen, &mut |leaf| {
            let mut changed = false;
            for (_, kind, offset, size) in items(leaf) {
                if kind != INODE_ITEM || size < GID_AT + 4 {
                    continue;
                }
                for field in [UID_AT, GID_AT] {
                    let at = offset + field;
                    if u32_at(leaf, at) != 0 {
                        leaf[at..at + 4].fill(0);
                        changed = true;
                    }
                }
            }
            Ok(changed)
        })?;
    }
    image.file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn makes_every_node_of_every_subvolume_owned_by_0() {
        let dir = crate::workspace_root().join("target/xtask-test/owners");
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join("sub/deep")).unwrap();
        for i in 0..300 {
            std::fs::write(tree.join(format!("sub/deep/f{i}")), b"x").unwrap();
        }
        std::fs::write(tree.join("top"), b"top").unwrap();
        let image = dir.join("owners.btrfs");
        File::create(&image).unwrap().set_len(256 << 20).unwrap();
        let made = Command::new("mkfs.btrfs")
            .args([
                "-q",
                "-f",
                "--csum",
                "crc32c",
                "--subvol",
                "sub",
                "--rootdir",
            ])
            .arg(&tree)
            .arg(&image)
            .status()
            .unwrap();
        assert!(made.success());
        make_owned_by_0(&image).unwrap();
        let checked = Command::new("btrfs")
            .args(["check", "--readonly"])
            .arg(&image)
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        // Every inode item of the top level and of the subvolume.
        for tree in ["5", "256"] {
            let dumped = Command::new("btrfs")
                .args(["inspect-internal", "dump-tree", "-t", tree])
                .arg(&image)
                .output()
                .unwrap();
            let dumped = String::from_utf8_lossy(&dumped.stdout);
            let owners: Vec<&str> = dumped.lines().filter(|l| l.contains(" uid ")).collect();
            assert!(!owners.is_empty(), "no inode items in tree {tree}");
            assert!(
                owners.iter().all(|l| l.contains("uid 0 gid 0")),
                "tree {tree}: {owners:?}"
            );
        }
    }
}
