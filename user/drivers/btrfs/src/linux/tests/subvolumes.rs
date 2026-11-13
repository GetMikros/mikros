// SPDX-License-Identifier: GPL-2.0
//! Managing subvolumes through the shim.

use super::{Image, start};
use crate::linux::Mount;
use crate::linux::host::Disk;
use crate::linux::node::{Kind, Node};
use std::vec::Vec;

const EEXIST: i32 = -17;
const EINVAL: i32 = -22;
const EROFS: i32 = -30;

/// The paths, IDs and read-only flags of every subvolume.
fn list(root: &Node<'_>) -> Vec<(Vec<u8>, u64, bool)> {
    let mut all = Vec::new();
    let mut cursor = 0;
    root.subvolumes(&mut cursor, |path, id, read_only| {
        all.push((path.to_vec(), id, read_only));
        true
    })
    .unwrap();
    all
}

/// A subvolume made in a directory is listed with its path from the top,
/// and a read-only snapshot of it holds its file and cannot be written
/// until it is made writable; a list goes on from its cursor; a directory
/// is no subvolume's root; and removed subvolumes are no longer listed.
#[test]
fn manages_subvolumes() {
    start();
    let image = Image::new("subvolumes", 256 << 20, &[]);
    Disk::add(c"subvolumes", image.open(), false).unwrap();
    {
        let mount = Mount::new(c"/dev/subvolumes", false, None).unwrap();
        let root = mount.root().unwrap();
        let dir = root.create(b"d", Kind::Directory, 0o755).unwrap();
        dir.create_subvolume(b"a").unwrap();
        assert_eq!(dir.create_subvolume(b"a"), Err(EEXIST));
        {
            let a = dir.lookup(b"a").unwrap();
            let file = a.create(b"f", Kind::File, 0o644).unwrap();
            assert_eq!(file.write(0, b"in a"), Ok(4));
            root.snapshot(&a, b"snap", true).unwrap();
        }
        let all = list(&root);
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all.iter().any(|(p, _, ro)| p == b"d/a" && !ro));
        assert!(all.iter().any(|(p, _, ro)| p == b"snap" && *ro));
        assert!(all.windows(2).all(|w| w[0].1 < w[1].1));

        let mut cursor = 0;
        let mut one = Vec::new();
        root.subvolumes(&mut cursor, |_, id, _| {
            one.push(id);
            one.len() < 2
        })
        .unwrap();
        assert_eq!((one.len(), cursor), (2, all[0].1));
        let mut rest = Vec::new();
        root.subvolumes(&mut cursor, |_, id, _| {
            rest.push(id);
            true
        })
        .unwrap();
        assert_eq!(rest, [all[1].1]);

        {
            let snap = root.lookup(b"snap").unwrap();
            let mut buf = [0; 4];
            assert_eq!(snap.lookup(b"f").unwrap().read(0, &mut buf), Ok(4));
            assert!(&buf == b"in a");
            assert_eq!(snap.create(b"g", Kind::File, 0o644).err(), Some(EROFS));
            snap.set_read_only(false).unwrap();
            snap.create(b"g", Kind::File, 0o644).unwrap();
            assert!(list(&root).iter().any(|(p, _, ro)| p == b"snap" && !ro));
        }
        assert_eq!(dir.set_read_only(true), Err(EINVAL));

        root.delete_subvolume(b"snap").unwrap();
        dir.delete_subvolume(b"a").unwrap();
        assert!(list(&root).is_empty());
    }
    image.check();
}
