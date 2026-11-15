use super::file::EXTENT_DATA;
use super::zstd::{COMPRESS_ZSTD, decompress};
use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::string::String;

/// A file system made by mkfs.btrfs from the tree that `fill` writes,
/// with `args` added to its command line.
pub(crate) fn mkfs(name: &str, args: &[&str], fill: impl FnOnce(&Path)) -> Vec<u8> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("mikrosboot-btrfs-{pid}-{n}-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let tree = dir.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    fill(&tree);
    let image: PathBuf = dir.join("image");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(256 << 20)
        .unwrap();
    let status = Command::new("mkfs.btrfs")
        .args(["-q", "-f", "--rootdir"])
        .arg(&tree)
        .args(args)
        .arg(&image)
        .status()
        .expect("mkfs.btrfs is needed for these tests");
    assert!(status.success(), "mkfs.btrfs failed");
    let bytes = std::fs::read(&image).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    bytes
}

/// Bytes that do not compress.
fn noise(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

pub(crate) fn write(tree: &Path, path: &str, bytes: &[u8]) {
    let path = tree.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn walk(v: &mut Volume<Vec<u8>>, path: &str) -> Node {
    let mut node = v.top().unwrap();
    for name in path.split('/') {
        node = v.lookup(node, name.as_bytes()).unwrap().expect(name);
    }
    node
}

fn plain() -> Vec<u8> {
    mkfs("plain", &[], |t| {
        write(t, "small", b"hello\n");
        write(t, "big", &noise(300 * 1024 + 17));
        write(t, "empty", b"");
        write(t, "dir/sub/file", b"deep\n");
        std::os::unix::fs::symlink("dir/sub/file", t.join("link")).unwrap();
    })
}

#[test]
fn finds_entries_and_inodes() {
    let mut v = Volume::open(plain()).unwrap();
    let small = walk(&mut v, "small");
    assert_eq!(v.inode(small).unwrap().kind, Kind::File);
    assert_eq!(v.inode(small).unwrap().size, 6);
    let big = walk(&mut v, "big");
    assert_eq!(v.inode(big).unwrap().size, 300 * 1024 + 17);
    let file = walk(&mut v, "dir/sub/file");
    assert_eq!(v.inode(file).unwrap().kind, Kind::File);
    let dir = walk(&mut v, "dir");
    assert_eq!(v.inode(dir).unwrap().kind, Kind::Directory);
    let link = walk(&mut v, "link");
    assert_eq!(v.inode(link).unwrap().kind, Kind::Symlink);
    assert_eq!(v.lookup(dir, b"missing").unwrap(), None);
    let top = v.top().unwrap();
    let mut names: Vec<String> = v
        .entries(top)
        .unwrap()
        .into_iter()
        .map(|(name, _)| String::from_utf8(name).unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["big", "dir", "empty", "link", "small"]);
    assert!(v.is_subvolume_root(top).unwrap());
    assert!(!v.is_subvolume_root(dir).unwrap());
}

fn nested() -> Vec<u8> {
    let subvolumes = ["--subvol", "outer", "--subvol", "ro:outer/inner"];
    mkfs("subvol", &subvolumes, |t| {
        write(t, "outer/inner/file", b"inside\n");
        write(t, "outer/other", b"beside\n");
    })
}

#[test]
fn follows_entries_into_subvolumes() {
    let mut v = Volume::open(nested()).unwrap();
    let top = v.top().unwrap();
    let outer = walk(&mut v, "outer");
    let inner = walk(&mut v, "outer/inner");
    assert_ne!(outer.subvolume, top.subvolume);
    assert_ne!(inner.subvolume, outer.subvolume);
    assert!(v.is_subvolume_root(inner).unwrap());
    let file = walk(&mut v, "outer/inner/file");
    assert_eq!(file.subvolume, inner.subvolume);
    assert_eq!(v.inode(file).unwrap().size, 7);
    let listed = v.entries(outer).unwrap();
    assert!(
        listed
            .iter()
            .any(|(name, node)| name == b"inner" && *node == inner)
    );
}

#[test]
fn checks_tree_block_checksums() {
    let image = plain();
    let mut v = Volume::open(image.clone()).unwrap();
    let (root, _) = v.root_item(5).unwrap().unwrap();
    let all = |kind| Key {
        objectid: if kind == 0 { 0 } else { u64::MAX },
        kind,
        offset: if kind == 0 { 0 } else { u64::MAX },
    };
    let mut items = Vec::new();
    v.search(root, all(0), all(u8::MAX), &mut items).unwrap();
    assert!(items.len() > 10);
    let physical = v.map(root.bytenr, 1).unwrap() as usize;
    let mut bad = image;
    bad[physical + HEADER + 3] ^= 1;
    let mut v = Volume::open(bad).unwrap();
    assert_eq!(
        v.search(root, all(0), all(u8::MAX), &mut Vec::new()).err(),
        Some(Error::TreeChecksum(root.bytenr))
    );
}

fn patch_superblock(image: &mut [u8], change: impl FnOnce(&mut [u8])) {
    let sb = &mut image[SUPERBLOCK_AT as usize..SUPERBLOCK_AT as usize + SUPERBLOCK_SIZE];
    change(sb);
    let sum = crc32c(&sb[CSUM_SIZE..]);
    sb[..4].copy_from_slice(&sum.to_le_bytes());
}

#[test]
fn checks_the_superblock() {
    let image = plain();
    let mut bad = image.clone();
    bad[SUPERBLOCK_AT as usize + 0x40] = b'x';
    assert_eq!(Volume::open(bad).err(), Some(Error::Magic));
    let mut bad = image.clone();
    bad[SUPERBLOCK_AT as usize + 0x12b] ^= 1;
    assert_eq!(Volume::open(bad).err(), Some(Error::SuperblockChecksum));
    let mut bad = image.clone();
    patch_superblock(&mut bad, |sb| sb[0x88] = 2);
    assert_eq!(Volume::open(bad).err(), Some(Error::Devices(2)));
    let mut bad = image.clone();
    patch_superblock(&mut bad, |sb| sb[0xbc] |= 0x80);
    assert_eq!(Volume::open(bad).err(), Some(Error::Incompat(0x80)));
    let mut bad = image.clone();
    patch_superblock(&mut bad, |sb| sb[0xc4] = 1);
    assert_eq!(Volume::open(bad).err(), Some(Error::ChecksumType(1)));
    // Flags that are not incompatible do not matter.
    let mut fine = image;
    patch_superblock(&mut fine, |sb| sb[0xb4..0xbc].fill(0xff));
    assert!(Volume::open(fine).is_ok());
}

#[test]
fn maps_only_single_and_dup_chunks() {
    let mut v = Volume::open(plain()).unwrap();
    v.chunks = vec![
        Chunk {
            logical: 0x10000,
            length: 0x1000,
            kind: 1,
            physical: 0x50000,
        },
        Chunk {
            logical: 0x20000,
            length: 0x1000,
            kind: 4 | DUP,
            physical: 0x60000,
        },
        Chunk {
            logical: 0x30000,
            length: 0x1000,
            kind: 1 | (1 << 4),
            physical: 0x70000,
        },
    ];
    assert_eq!(v.map(0x10010, 16), Ok(0x50010));
    assert_eq!(v.map(0x20ff0, 16), Ok(0x60ff0));
    assert_eq!(v.map(0x30000, 16), Err(Error::Profile(0x30000)));
    assert_eq!(v.map(0x40000, 16), Err(Error::Unmapped(0x40000)));
    assert!(matches!(v.map(0x10ff8, 16), Err(Error::Corrupt(_))));
}

fn contents(v: &mut Volume<Vec<u8>>, node: Node) -> Vec<u8> {
    let mut out = vec![0; v.inode(node).unwrap().size as usize];
    v.read(node, &mut out).unwrap();
    out
}

#[test]
fn reads_files_and_links() {
    let mut v = Volume::open(plain()).unwrap();
    for (path, expected) in [
        ("small", b"hello\n".to_vec()),
        ("big", noise(300 * 1024 + 17)),
        ("empty", Vec::new()),
        ("dir/sub/file", b"deep\n".to_vec()),
    ] {
        let node = walk(&mut v, path);
        assert_eq!(contents(&mut v, node), expected, "{path}");
    }
    let link = walk(&mut v, "link");
    assert_eq!(v.read_link(link).unwrap(), b"dir/sub/file");
    let file = walk(&mut v, "small");
    assert!(matches!(v.read_link(file), Err(Error::Corrupt(_))));
    let mut wrong = vec![0; 5];
    assert!(matches!(v.read(file, &mut wrong), Err(Error::Corrupt(_))));
}

#[test]
fn reads_files_in_subvolumes() {
    let mut v = Volume::open(nested()).unwrap();
    let file = walk(&mut v, "outer/inner/file");
    assert_eq!(contents(&mut v, file), b"inside\n");
}

#[test]
fn checks_data_checksums() {
    let image = plain();
    let mut v = Volume::open(image.clone()).unwrap();
    let big = walk(&mut v, "big");
    let (_, item) = v.items(big, EXTENT_DATA).unwrap().remove(0);
    let disk_bytenr = u64_at(&item, 21);
    let physical = v.map(disk_bytenr, 1).unwrap() as usize;
    let mut bad = image;
    bad[physical + 4096 + 5] ^= 1;
    let mut v = Volume::open(bad).unwrap();
    let big = walk(&mut v, "big");
    let mut out = vec![0; v.inode(big).unwrap().size as usize];
    assert_eq!(
        v.read(big, &mut out),
        Err(Error::DataChecksum(disk_bytenr + 4096))
    );
}

/// Bytes that compress well.
fn text(len: usize) -> Vec<u8> {
    let line = b"the quick brown fox jumps over the lazy dog 0123456789\n";
    line.iter().copied().cycle().take(len).collect()
}

#[test]
fn decompresses_zstd_extents() {
    let image = mkfs("zstd", &["--compress", "zstd"], |t| {
        write(t, "inline", &text(1500));
        write(t, "big", &text(700 * 1024 + 3));
        write(t, "noise", &noise(200 * 1024));
    });
    let mut v = Volume::open(image).unwrap();
    for (path, expected) in [
        ("inline", text(1500)),
        ("big", text(700 * 1024 + 3)),
        ("noise", noise(200 * 1024)),
    ] {
        let node = walk(&mut v, path);
        assert_eq!(contents(&mut v, node), expected, "{path}");
    }
    // The large file is stored compressed.
    let big = walk(&mut v, "big");
    let extents = v.items(big, EXTENT_DATA).unwrap();
    assert!(extents.iter().any(|(_, item)| item[16] == COMPRESS_ZSTD));
}

#[test]
fn refuses_other_compression() {
    let image = mkfs("lzo", &["--compress", "lzo"], |t| {
        write(t, "big", &text(400 * 1024))
    });
    assert_eq!(Volume::open(image).err(), Some(Error::Incompat(1 << 3)));
    assert_eq!(decompress(1, b"x", 1), Err(Error::Compression(1)));
}
