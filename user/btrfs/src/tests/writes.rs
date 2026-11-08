// SPDX-License-Identifier: GPL-2.0
//! Changing a btrfs through the shim, which the host's btrfs check must
//! find sound once it is unmounted, and which a read-only mount that
//! follows must read as it was made.

use super::{Image, start};
use crate::Mount;
use crate::host::Disk;
use crate::node::{Kind, MODE_DIRECTORY, MODE_FILE, MODE_SYMLINK, MODE_TYPE, Node};
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::format;
use std::vec::Vec;

const ENOENT: i32 = -2;
const EEXIST: i32 = -17;
const ENOTDIR: i32 = -20;
const EISDIR: i32 = -21;
const EROFS: i32 = -30;
const ENOTEMPTY: i32 = -39;

/// Makes a btrfs of `size` bytes, gives it to the shim as /dev/`name`,
/// mounts it with `options`, and calls `change` with its root and then
/// `check`. Then it unmounts it, asserts that btrfs check finds it sound,
/// hands the image to `after`, mounts it read-only and calls `check`
/// again.
fn changes(
    name: &str,
    size: u64,
    options: Option<&CStr>,
    change: impl FnOnce(&Node<'_>),
    check: impl Fn(&Node<'_>),
    after: impl FnOnce(&Image),
) {
    start();
    let image = Image::new(name, size, &[]);
    let disk = CString::new(name).unwrap();
    Disk::add(&disk, image.open(), false).unwrap();
    let source = CString::new(format!("/dev/{name}")).unwrap();
    let mount = Mount::new(&source, false, options).unwrap();
    {
        let root = mount.root().unwrap();
        change(&root);
        check(&root);
    }
    drop(mount);
    image.check();
    after(&image);
    let mount = Mount::new(&source, true, None).unwrap();
    let root = mount.root().unwrap();
    check(&root);
    assert_eq!(root.create(b"new", Kind::File, 0o644).err(), Some(EROFS));
}

fn walk<'m>(root: &Node<'m>, path: &str) -> Node<'m> {
    let mut names = path.split('/');
    let mut node = root.lookup(names.next().unwrap().as_bytes()).unwrap();
    for name in names {
        node = node.lookup(name.as_bytes()).unwrap();
    }
    node
}

fn mode(root: &Node<'_>, path: &str) -> u32 {
    walk(root, path).attributes().unwrap().mode
}

/// The names in a directory but "." and "..".
fn names(dir: &Node<'_>) -> BTreeSet<Vec<u8>> {
    let mut names = BTreeSet::new();
    let mut cookie = 0;
    loop {
        let mut more = false;
        dir.read_dir(&mut cookie, |name, _, _| {
            more = true;
            if name != b"." && name != b".." {
                names.insert(name.to_vec());
            }
            true
        })
        .unwrap();
        if !more {
            return names;
        }
    }
}

fn many() -> BTreeSet<Vec<u8>> {
    (0..1000).map(|i| format!("f{i:04}").into_bytes()).collect()
}

#[test]
fn creates_and_removes_entries() {
    changes(
        "creates-and-removes",
        256 << 20,
        None,
        |root| {
            let d = root.create(b"d", Kind::Directory, 0o755).unwrap();
            d.create(b"f", Kind::File, 0o640).unwrap();
            root.create(b"s", Kind::Symlink(c"d/f"), 0).unwrap();
            assert_eq!(root.create(b"d", Kind::File, 0o644).err(), Some(EEXIST));
            root.create(b"e", Kind::Directory, 0o755).unwrap();
            assert_eq!(root.remove(b"e", false).err(), Some(EISDIR));
            root.remove(b"e", true).unwrap();
            assert_eq!(root.remove(b"d", true).err(), Some(ENOTEMPTY));
            assert_eq!(root.remove(b"s", true).err(), Some(ENOTDIR));
            root.create(b"gone", Kind::File, 0o644).unwrap();
            root.remove(b"gone", false).unwrap();
            assert_eq!(root.remove(b"gone", false).err(), Some(ENOENT));
            let m = root.create(b"m", Kind::Directory, 0o700).unwrap();
            for name in many() {
                m.create(&name, Kind::File, 0o600).unwrap();
            }
        },
        |root| {
            assert_eq!(mode(root, "d"), MODE_DIRECTORY | 0o755);
            assert_eq!(mode(root, "d/f"), MODE_FILE | 0o640);
            assert_eq!(walk(root, "d/f").attributes().unwrap().size, 0);
            let s = walk(root, "s");
            assert_eq!(s.attributes().unwrap().mode & MODE_TYPE, MODE_SYMLINK);
            let mut target = [0; 16];
            let len = s.read_link(&mut target).unwrap();
            assert_eq!(&target[..len], b"d/f");
            for name in ["e", "gone"] {
                assert_eq!(root.lookup(name.as_bytes()).err(), Some(ENOENT));
            }
            assert_eq!(names(&walk(root, "m")), many());
        },
        |_| {},
    );
}
