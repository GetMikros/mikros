// SPDX-License-Identifier: GPL-2.0
//! Reading the files and directories of an image that mkfs.btrfs made
//! from a tree of the host, and comparing them with the tree.

use super::{image, start};
use crate::host::Disk;
use crate::node::{MODE_DIRECTORY, MODE_FILE, MODE_TYPE, Node};
use std::ffi::{CString, OsStr};
use std::format;
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

const ENOENT: i32 = -2;
const ENOTDIR: i32 = -20;
const EACCES: i32 = -13;
const EISDIR: i32 = -21;

/// The most a read of the file protocol asks for.
const CHUNK: usize = 32768;

/// Bytes that do not compress, from xorshift64 seeded with `seed`.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Text that compresses well.
fn text(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut line = 0;
    while out.len() < len {
        out.extend_from_slice(format!("line {line} of a file that compresses well\n").as_bytes());
        line += 1;
    }
    out.truncate(len);
    out
}

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

/// A tree of the host with `files()`, removed when dropped.
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
        Tree(root)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Mounts, read-only, an image of a tree made with the mkfs.btrfs
/// options `options`, and checks what it reads against the tree.
fn reads(name: &str, options: &[&OsStr]) {
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
    for path in ["dir", "dir/nested", "many"] {
        let dir = walk(&root, path);
        assert_eq!(dir.attributes().unwrap().mode & MODE_TYPE, MODE_DIRECTORY);
        assert_eq!(dir.read(0, &mut [0; 8]), Err(EISDIR));
    }
    check_lookups(&root);
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

#[test]
fn reads_files() {
    reads("reads-files", &[]);
}

#[test]
fn reads_compressed_files() {
    reads(
        "reads-compressed",
        &[OsStr::new("--compress"), OsStr::new("zstd")],
    );
}
