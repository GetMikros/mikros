use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::string::String;

/// A file system made by mkfs.btrfs from the tree that `fill` writes,
/// with `args` added to its command line.
fn mkfs(name: &str, args: &[&str], fill: impl FnOnce(&Path)) -> Vec<u8> {
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

fn write(tree: &Path, path: &str, bytes: &[u8]) {
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
