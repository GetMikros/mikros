// SPDX-License-Identifier: GPL-2.0
//! Reading the files, links and directories of an image that mkfs.btrfs
//! made from a tree of the host, and comparing them with the tree.

use super::{image, noise, start, text};
use crate::host::Disk;
use crate::node::{
    ENTRY_DIRECTORY, ENTRY_FILE, ENTRY_SYMLINK, MODE_DIRECTORY, MODE_FILE, MODE_SYMLINK, MODE_TYPE,
    Node,
};
use std::collections::BTreeSet;
use std::ffi::{CString, OsStr};
use std::format;
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

const ENOENT: i32 = -2;
const ENOTDIR: i32 = -20;
const EACCES: i32 = -13;
const EISDIR: i32 = -21;
const EINVAL: i32 = -22;

/// The most a read of the file protocol asks for.
const CHUNK: usize = 32768;

/// The files of the tree, by their paths.
fn files() -> Vec<(String, Vec<u8>)> {
    let mut files = std::vec![
        ("empty".into(), Vec::new()),
        ("small".into(), noise(1, 100)),
        ("page".into(), noise(2, 4096)),
        ("medium".into(), noise(3, 100_000)),
        ("large".into(), noise(4, 5 << 20)),
        ("text".into(), text(3 << 20)),
        ("dir/nested/deep".into(), noise(5, 3000)),
    ];
    for i in 0..300 {
        files.push((format!("many/f{i:03}"), noise(100 + i, 10)));
    }
    files
}

/// The symbolic links of the tree, and their targets.
const LINKS: &[(&str, &str)] = &[("link", "small"), ("dir/up", "../small")];

/// A tree of the host with `files()` and `LINKS`, removed when dropped.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("mikros-btrfs-tree-{name}-{}", std::process::id()));
        for (path, data) in files() {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        for (path, target) in LINKS {
            std::os::unix::fs::symlink(target, root.join(path)).unwrap();
        }
        Tree(root)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Mounts, read-only, an image of a tree made with the mkfs.btrfs
/// options `options`, checks what it reads against the tree, and calls
/// `more` with the root for checks of its own.
fn reads(name: &str, options: &[&OsStr], more: impl FnOnce(&Node<'_>)) {
    start();
    let tree = Tree::new(name);
    let mut all: Vec<&OsStr> = std::vec![OsStr::new("--rootdir"), tree.0.as_os_str()];
    all.extend_from_slice(options);
    let disk = CString::new(name).unwrap();
    Disk::add(&disk, image(name, 256 << 20, &all), true).unwrap();
    drop(tree);
    let source = CString::new(format!("/dev/{name}")).unwrap();
    let mount = crate::Mount::new(&source, true, None).unwrap();
    let root = mount.root().unwrap();
    for (path, data) in files() {
        check_file(&walk(&root, &path), &path, &data);
    }
    for (path, target) in LINKS {
        let link = walk(&root, path);
        assert_eq!(link.attributes().unwrap().mode & MODE_TYPE, MODE_SYMLINK);
        let mut buf = [0; 64];
        let len = link.read_link(&mut buf).unwrap();
        assert_eq!(&buf[..len], target.as_bytes(), "{path}");
        assert_eq!(link.read(0, &mut buf), Err(EINVAL));
    }
    for path in ["dir", "dir/nested", "many"] {
        let dir = walk(&root, path);
        assert_eq!(dir.attributes().unwrap().mode & MODE_TYPE, MODE_DIRECTORY);
        assert_eq!(dir.read(0, &mut [0; 8]), Err(EISDIR));
    }
    check_lookups(&root);
    check_listing(&root);
    more(&root);
}

/// The node at `path`, looked up one name at a time.
fn walk<'m>(root: &Node<'m>, path: &str) -> Node<'m> {
    let mut names = path.split('/');
    let mut node = root.lookup(names.next().unwrap().as_bytes()).unwrap();
    for name in names {
        node = node.lookup(name.as_bytes()).unwrap();
    }
    node
}

fn check_file(file: &Node<'_>, path: &str, data: &[u8]) {
    let attributes = file.attributes().unwrap();
    assert_eq!(attributes.mode & MODE_TYPE, MODE_FILE, "{path}");
    assert_eq!(attributes.size, data.len() as u64, "{path}");
    assert_eq!(attributes.links, 1, "{path}");
    // The whole file, as a client of the file protocol reads it.
    let mut read = Vec::new();
    let mut buf = std::vec![0; CHUNK];
    loop {
        let len = file.read(read.len() as u64, &mut buf).unwrap();
        if len == 0 {
            break;
        }
        read.extend_from_slice(&buf[..len]);
    }
    assert!(read == data, "{path} reads as it was written");
    // A read that starts and ends inside blocks.
    if data.len() > 20_000 {
        let len = file.read(12_345, &mut buf[..5_000]).unwrap();
        assert_eq!(&buf[..len], &data[12_345..17_345], "{path}");
    }
    assert_eq!(file.read(data.len() as u64 + 1, &mut buf), Ok(0), "{path}");
}

fn check_lookups(root: &Node<'_>) {
    assert_eq!(root.lookup(b"missing").err(), Some(ENOENT));
    assert_eq!(walk(root, "small").lookup(b"x").err(), Some(ENOTDIR));
    for name in [&b"."[..], b"..", b"a/b", b"a\0b"] {
        assert_eq!(root.lookup(name).err(), Some(EACCES), "{name:?}");
    }
}

/// The entries of `dir` but "." and "..", by name, taking at most
/// `batch` entries from each call, as a client with a small buffer would.
fn list(dir: &Node<'_>, batch: usize) -> Vec<(Vec<u8>, u64, u32)> {
    let mut entries = Vec::new();
    let mut cookie = 0;
    loop {
        let mut taken = 0;
        dir.read_dir(&mut cookie, |name, inode, kind| {
            if taken == batch {
                return false;
            }
            taken += 1;
            if name != b"." && name != b".." {
                entries.push((name.to_vec(), inode, kind));
            }
            true
        })
        .unwrap();
        if taken == 0 {
            return entries;
        }
    }
}

fn check_listing(root: &Node<'_>) {
    let names = |entries: &[(Vec<u8>, u64, u32)]| -> BTreeSet<Vec<u8>> {
        entries.iter().map(|e| e.0.clone()).collect()
    };
    let top = list(root, 64);
    let expected: BTreeSet<Vec<u8>> = [
        "empty", "small", "page", "medium", "large", "text", "dir", "many", "link",
    ]
    .iter()
    .map(|n| n.as_bytes().to_vec())
    .collect();
    assert_eq!(names(&top), expected);
    for (name, inode, kind) in &top {
        let node = root.lookup(name).unwrap();
        let attributes = node.attributes().unwrap();
        assert_eq!(*inode, attributes.inode);
        let mode = match *kind {
            ENTRY_FILE => MODE_FILE,
            ENTRY_DIRECTORY => MODE_DIRECTORY,
            ENTRY_SYMLINK => MODE_SYMLINK,
            other => panic!("entry type {other}"),
        };
        assert_eq!(attributes.mode & MODE_TYPE, mode);
    }
    // A directory longer than any batch, listed a few entries at a time.
    let many = walk(root, "many");
    let entries = list(&many, 7);
    assert_eq!(entries.len(), 300, "no entry twice");
    let expected: BTreeSet<Vec<u8>> = (0..300).map(|i| format!("f{i:03}").into_bytes()).collect();
    assert_eq!(names(&entries), expected);
    // The same position gives the same entries again.
    assert_eq!(list(&many, 300), list(&many, 300));
}

#[test]
fn reads_files() {
    reads("reads-files", &[], |_| {});
}

/// With "dir" a subvolume, which lookups cross into.
#[test]
fn reads_files_of_subvolumes() {
    reads(
        "reads-subvolumes",
        &[OsStr::new("--subvol"), OsStr::new("rw:dir")],
        |root| {
            // The root directory of every subvolume is inode 256.
            let dir = root.lookup(b"dir").unwrap();
            assert_eq!(dir.attributes().unwrap().inode, 256);
        },
    );
}

#[test]
fn reads_compressed_files() {
    reads(
        "reads-compressed",
        &[OsStr::new("--compress"), OsStr::new("zstd")],
        |_| {},
    );
}
