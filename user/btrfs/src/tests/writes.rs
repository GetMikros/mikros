// SPDX-License-Identifier: GPL-2.0
//! Changing a btrfs through the shim: entries, links, file contents and
//! attributes, which the host's btrfs check must find sound once it is
//! unmounted, and which a read-only mount that follows must read as they
//! were made.

use super::{Image, noise, start, text};
use crate::Mount;
use crate::host::Disk;
use crate::node::{
    Changes, Kind, MODE_DIRECTORY, MODE_FILE, MODE_SYMLINK, MODE_TYPE, Node, RENAME_EXCHANGE,
    RENAME_NOREPLACE, Time,
};
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::format;
use std::vec::Vec;

const EPERM: i32 = -1;
const ENOENT: i32 = -2;
const EEXIST: i32 = -17;
const ENOTDIR: i32 = -20;
const EISDIR: i32 = -21;
const EINVAL: i32 = -22;
const EROFS: i32 = -30;
const ENOTEMPTY: i32 = -39;

/// The most a read or write of the file protocol carries.
const CHUNK: usize = 32768;

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

#[test]
fn renames_and_links() {
    changes(
        "renames-and-links",
        256 << 20,
        None,
        |root| {
            let d = root.create(b"d", Kind::Directory, 0o755).unwrap();
            let a = d.create(b"a", Kind::File, 0o644).unwrap();
            root.create(b"b", Kind::File, 0o600).unwrap();
            a.link(root, b"hard").unwrap();
            assert_eq!(a.link(root, b"b").err(), Some(EEXIST));
            assert_eq!(d.link(root, b"d2").err(), Some(EPERM));
            root.rename(b"b", &d, b"b2", 0).unwrap();
            assert_eq!(
                d.rename(b"b2", root, b"hard", RENAME_NOREPLACE).err(),
                Some(EEXIST)
            );
            // "hard" and "d/b2" swap their nodes.
            root.rename(b"hard", &d, b"b2", RENAME_EXCHANGE).unwrap();
            assert_eq!(
                root.rename(b"hard", &d, b"none", RENAME_EXCHANGE).err(),
                Some(ENOENT)
            );
            // A directory cannot move below itself, nor replace one that
            // is not empty.
            assert_eq!(root.rename(b"d", &d, b"x", 0).err(), Some(EINVAL));
            root.create(b"empty", Kind::Directory, 0o755).unwrap();
            assert_eq!(root.rename(b"empty", root, b"d", 0).err(), Some(ENOTEMPTY));
        },
        |root| {
            let a = walk(root, "d/a").attributes().unwrap();
            assert_eq!(a.links, 2);
            // After the exchange, "d/b2" is the link to "d/a", and
            // "hard" what was "b".
            assert_eq!(walk(root, "d/b2").attributes().unwrap().inode, a.inode);
            assert_eq!(mode(root, "hard"), MODE_FILE | 0o600);
            assert_eq!(root.lookup(b"b").err(), Some(ENOENT));
            let expected: BTreeSet<Vec<u8>> = [&b"d"[..], b"hard", b"empty"]
                .iter()
                .map(|n| n.to_vec())
                .collect();
            assert_eq!(names(root), expected);
        },
        |_| {},
    );
}

fn read_all(file: &Node<'_>) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = std::vec![0; CHUNK];
    loop {
        let len = file.read(data.len() as u64, &mut buf).unwrap();
        if len == 0 {
            return data;
        }
        data.extend_from_slice(&buf[..len]);
    }
}

/// Writes `data` at `offset` in the file protocol's pieces.
fn write_all(file: &Node<'_>, offset: u64, data: &[u8]) {
    for (i, piece) in data.chunks(CHUNK).enumerate() {
        let written = file.write(offset + (i * CHUNK) as u64, piece).unwrap();
        assert_eq!(written, piece.len());
    }
}

/// What `writes_files` leaves in "a": written, overwritten in the
/// middle, appended to, truncated and extended with zeros.
fn contents() -> Vec<u8> {
    let mut data = noise(1, 300_000);
    data[100_000..150_000].copy_from_slice(&noise(2, 50_000));
    data.extend_from_slice(&noise(3, 10_000));
    data.truncate(250_000);
    data.resize(260_000, 0);
    data
}

const TIMES: (Time, Time) = (
    Time::new(1_000_000_000, 123),
    Time::new(1_500_000_000, 456_789),
);

#[test]
fn writes_files() {
    changes(
        "writes-files",
        256 << 20,
        None,
        |root| {
            let a = root.create(b"a", Kind::File, 0o644).unwrap();
            write_all(&a, 0, &noise(1, 300_000));
            write_all(&a, 100_000, &noise(2, 50_000));
            assert_eq!(a.append(&noise(3, 10_000)), Ok((10_000, 300_000)));
            a.truncate(250_000).unwrap();
            a.truncate(260_000).unwrap();
            a.set_attributes(&Changes {
                mode: Some(0o600),
                accessed: Some(TIMES.0),
                modified: Some(TIMES.1),
                owner: Some(1000),
                group: Some(1001),
            })
            .unwrap();
            let attributes = a.attributes().unwrap();
            assert_eq!((attributes.accessed, attributes.modified), TIMES);
            a.sync().unwrap();
            // Small enough to be inline in the file system's metadata.
            let small = root.create(b"small", Kind::File, 0o644).unwrap();
            write_all(&small, 0, &noise(4, 50));
            small.sync().unwrap();
            let m = root.create(b"m", Kind::Directory, 0o755).unwrap();
            for (i, name) in many().iter().enumerate() {
                let f = m.create(name, Kind::File, 0o644).unwrap();
                write_all(&f, 0, &noise(10 + i as u64, 100 + i));
            }
            let d = root.create(b"d", Kind::Directory, 0o755).unwrap();
            assert_eq!(d.write(0, b"x").err(), Some(EISDIR));
        },
        |root| {
            let a = walk(root, "a");
            let attributes = a.attributes().unwrap();
            assert_eq!(attributes.mode, MODE_FILE | 0o600);
            // Reads move the access time on, as it is older than the
            // modification time.
            assert_eq!(attributes.modified, TIMES.1);
            assert_eq!((attributes.owner, attributes.group), (1000, 1001));
            assert_eq!(attributes.size, 260_000);
            assert!(read_all(&a) == contents());
            assert_eq!(read_all(&walk(root, "small")), noise(4, 50));
            let m = walk(root, "m");
            for (i, name) in many().iter().enumerate() {
                let f = m.lookup(name).unwrap();
                assert_eq!(read_all(&f), noise(10 + i as u64, 100 + i));
            }
        },
        |_| {},
    );
}

/// The extents of an image's files that zstd compressed, by the host's
/// btrfs inspect-internal.
fn zstd_extents(image: &Image) -> usize {
    let output = std::process::Command::new("btrfs")
        .args(["inspect-internal", "dump-tree", "-t", "fs"])
        .arg(&image.0)
        .output()
        .unwrap();
    assert!(output.status.success());
    std::string::String::from_utf8_lossy(&output.stdout)
        .matches("compression 3")
        .count()
}

#[test]
fn writes_compressed_files() {
    let data = text(8 << 20);
    changes(
        "writes-compressed",
        256 << 20,
        Some(c"compress=zstd"),
        |root| {
            let file = root.create(b"text", Kind::File, 0o644).unwrap();
            write_all(&file, 0, &data);
        },
        |root| assert!(read_all(&walk(root, "text")) == data),
        |image| assert!(zstd_extents(image) > 0, "the file is compressed"),
    );
}

/// Puts the number of MiB `i` of a file in each 4 KiB block of `buf`,
/// whose other bytes stay as they are.
fn tag(buf: &mut [u8], i: u64) {
    for block in buf.chunks_mut(4096) {
        block[..8].copy_from_slice(&i.to_le_bytes());
    }
}

/// More than the page cache may hold dirty with the tests' memory, so
/// that the writer waits for writeback.
#[test]
fn writes_more_than_the_dirty_limit() {
    const MIB: u64 = 384;
    changes(
        "writes-dirty-limit",
        1 << 30,
        None,
        |root| {
            let file = root.create(b"big", Kind::File, 0o644).unwrap();
            // The writer outpaces writeback only if making its data is
            // cheap.
            let mut buf = noise(7, 1 << 20);
            for i in 0..MIB {
                tag(&mut buf, i);
                assert_eq!(file.write(i << 20, &buf), Ok(1 << 20));
            }
        },
        |root| {
            let file = walk(root, "big");
            assert_eq!(file.attributes().unwrap().size, MIB << 20);
            let mut expected = noise(7, 1 << 20);
            let mut buf = std::vec![0; 1 << 20];
            for i in 0..MIB {
                tag(&mut expected, i);
                assert_eq!(file.read(i << 20, &mut buf), Ok(1 << 20));
                assert!(buf == expected, "MiB {i}");
            }
        },
        |_| {},
    );
}

/// A file that was synced survives a crash just after, which a copy of
/// the image made while it is still mounted stands for: mounting the
/// copy replays the log tree that the sync wrote.
#[test]
fn syncs_files_through_crashes() {
    start();
    let image = Image::new("syncs", 256 << 20, &[]);
    Disk::add(c"syncs", image.open(), false).unwrap();
    let crashed = Image::new("syncs-crashed", 256 << 20, &[]);
    let mount = Mount::new(c"/dev/syncs", false, None).unwrap();
    {
        let root = mount.root().unwrap();
        let d = root.create(b"d", Kind::Directory, 0o755).unwrap();
        let a = d.create(b"a", Kind::File, 0o644).unwrap();
        write_all(&a, 0, &noise(20, 200_000));
        a.sync().unwrap();
        std::fs::copy(&image.0, &crashed.0).unwrap();
    }
    drop(mount);
    Disk::add(c"syncs-crashed", crashed.open(), false).unwrap();
    {
        let mount = Mount::new(c"/dev/syncs-crashed", false, None).unwrap();
        let root = mount.root().unwrap();
        assert!(read_all(&walk(&root, "d/a")) == noise(20, 200_000));
    }
    crashed.check();
}
