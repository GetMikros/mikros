use super::*;
use alloc::string::ToString;
use alloc::vec;

type T = Tree<u32, ()>;

fn file(t: &mut T, dir: Id, name: &str) -> Id {
    t.create(dir, name, Kind::File(0), 0o644, 0, 1).unwrap()
}

fn mkdir(t: &mut T, dir: Id, name: &str) -> Id {
    t.create(
        dir,
        name,
        Kind::Directory(Directory::default()),
        0o755,
        0,
        1,
    )
    .unwrap()
}

fn link(t: &mut T, dir: Id, name: &str, target: &str) -> Id {
    t.create(dir, name, Kind::Symlink(target.to_string()), 0o777, 0, 1)
        .unwrap()
}

fn names(t: &T, dir: Id) -> Vec<String> {
    t.list(dir, 0, 100)
        .unwrap()
        .0
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn creates_and_finds_nodes() {
    let mut t = T::new(0o755, 0, 0);
    let a = mkdir(&mut t, ROOT, "a");
    let f = file(&mut t, a, "f");
    assert_eq!(t.lookup(a, "f"), Ok(f));
    assert_eq!(t.resolve(ROOT, "a/f", false), Ok(Resolved::Node(f)));
    assert_eq!(t.resolve(ROOT, "./a/./f", false), Ok(Resolved::Node(f)));
    assert_eq!(t.resolve(ROOT, "a/", false), Ok(Resolved::Node(a)));
    assert_eq!(
        t.resolve(ROOT, "a/new", false),
        Ok(Resolved::Missing {
            parent: a,
            name: "new".into()
        })
    );
    assert_eq!(t.resolve(ROOT, "b/new", false), Err(Error::NotFound));
    assert_eq!(t.resolve(ROOT, "a/f/x", false), Err(Error::NotDirectory));
    assert_eq!(t.create(a, "f", Kind::File(0), 0, 0, 1), Err(Error::Exists));
    assert_eq!(t.links(a), 2);
    assert_eq!(t.links(ROOT), 3);
    assert_eq!(t.links(f), 1);
}

#[test]
fn checks_paths() {
    let t = T::new(0o755, 0, 0);
    assert_eq!(t.resolve(ROOT, "", false), Err(Error::Invalid));
    assert_eq!(t.resolve(ROOT, "/a", false), Err(Error::Invalid));
    assert_eq!(t.resolve(ROOT, "a//b", false), Err(Error::Invalid));
    assert_eq!(t.resolve(ROOT, "a/../b", false), Err(Error::AccessDenied));
    let long = "x".repeat(256);
    assert_eq!(t.resolve(ROOT, &long, false), Err(Error::NameTooLong));
}

#[test]
fn follows_links_that_stay_inside() {
    let mut t = T::new(0o755, 0, 0);
    let a = mkdir(&mut t, ROOT, "a");
    let b = mkdir(&mut t, ROOT, "b");
    let f = file(&mut t, b, "f");
    link(&mut t, a, "to-f", "../b/f");
    link(&mut t, a, "out", "../../etc");
    link(&mut t, a, "abs", "/etc/passwd");
    link(&mut t, ROOT, "loop", "loop");
    let l = link(&mut t, ROOT, "to-b", "b");
    assert_eq!(t.resolve(ROOT, "a/to-f", false), Ok(Resolved::Node(f)));
    assert_eq!(t.resolve(ROOT, "to-b/f", false), Ok(Resolved::Node(f)));
    // From inside `a`, `../b` leaves it.
    assert_eq!(
        t.resolve(a, "to-f", false),
        Ok(Resolved::Redirect("../b/f".into()))
    );
    assert_eq!(
        t.resolve(ROOT, "a/out/x/y", false),
        Ok(Resolved::Redirect("../../etc/x/y".into()))
    );
    assert_eq!(
        t.resolve(ROOT, "a/abs", false),
        Ok(Resolved::Redirect("/etc/passwd".into()))
    );
    assert_eq!(t.resolve(ROOT, "to-b", true), Ok(Resolved::Node(l)));
    assert_eq!(t.resolve(ROOT, "loop", false), Err(Error::Loop));
}

#[test]
fn unlinks_and_forgets() {
    let mut t = T::new(0o755, 0, 0);
    let a = mkdir(&mut t, ROOT, "a");
    let f = file(&mut t, a, "f");
    assert_eq!(t.unlink(ROOT, "a", true, 2), Err(Error::NotEmpty));
    assert_eq!(t.unlink(ROOT, "a", false, 2), Err(Error::IsDirectory));
    assert_eq!(t.unlink(a, "f", true, 2), Err(Error::NotDirectory));
    t.link(a, "f", ROOT, "g", 2).unwrap();
    assert_eq!(t.links(f), 2);
    assert_eq!(
        t.unlink(a, "f", false, 3),
        Ok(Removed { id: f, last: false })
    );
    assert_eq!(
        t.unlink(ROOT, "g", false, 3),
        Ok(Removed { id: f, last: true })
    );
    // A node with no name stays until it is forgotten, as open files do.
    assert!(t.get(f).is_some());
    assert!(t.forget(f).is_some());
    assert!(t.get(f).is_none());
    assert_eq!(
        t.unlink(ROOT, "a", true, 4),
        Ok(Removed { id: a, last: true })
    );
    assert_eq!(t.link(ROOT, "x", ROOT, "y", 4), Err(Error::NotFound));
}

#[test]
fn renames_as_posix_does() {
    let mut t = T::new(0o755, 0, 0);
    let a = mkdir(&mut t, ROOT, "a");
    let b = mkdir(&mut t, a, "b");
    let f = file(&mut t, ROOT, "f");
    let g = file(&mut t, ROOT, "g");
    assert_eq!(t.rename(ROOT, "a", b, "x", 2), Err(Error::Invalid));
    assert_eq!(t.rename(ROOT, "f", ROOT, "a", 2), Err(Error::IsDirectory));
    assert_eq!(t.rename(ROOT, "a", ROOT, "f", 2), Err(Error::NotDirectory));
    // A file replaces a file.
    assert_eq!(t.rename(ROOT, "f", a, "g2", 2), Ok(None));
    assert_eq!(t.lookup(a, "g2"), Ok(f));
    assert_eq!(
        t.rename(a, "g2", ROOT, "g", 2),
        Ok(Some(Removed { id: g, last: true }))
    );
    assert_eq!(t.lookup(ROOT, "g"), Ok(f));
    assert_eq!(t.get(f).unwrap().names, vec![(ROOT, "g".to_string())]);
    // A directory replaces an empty one, but not one with entries.
    let c = mkdir(&mut t, ROOT, "c");
    assert_eq!(t.rename(ROOT, "c", ROOT, "a", 3), Err(Error::NotEmpty));
    assert_eq!(
        t.rename(a, "b", ROOT, "c", 3),
        Ok(Some(Removed { id: c, last: true }))
    );
    assert_eq!(t.lookup(ROOT, "c"), Ok(b));
    // Two names of one node stay as they are.
    t.link(ROOT, "g", ROOT, "h", 4).unwrap();
    assert_eq!(t.rename(ROOT, "g", ROOT, "h", 4), Ok(None));
    // Listings follow the order in which the entries got their names.
    assert_eq!(names(&t, ROOT), ["a", "g", "c", "h"]);
}

#[test]
fn lists_in_a_stable_order() {
    let mut t = T::new(0o755, 0, 0);
    for name in ["c", "a", "b"] {
        file(&mut t, ROOT, name);
    }
    let (first, next) = t.list(ROOT, 0, 2).unwrap();
    assert_eq!(
        first.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["c", "a"]
    );
    // Entries that come and go before the cookie do not shift the rest.
    t.unlink(ROOT, "c", false, 2).unwrap();
    file(&mut t, ROOT, "d");
    let (rest, next) = t.list(ROOT, next, 10).unwrap();
    assert_eq!(
        rest.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["b", "d"]
    );
    assert!(t.list(ROOT, next, 10).unwrap().0.is_empty());
}

#[test]
fn ancestors_lead_to_the_root() {
    let mut t = T::new(0o755, 0, 1);
    let a = mkdir(&mut t, ROOT, "a");
    let b = mkdir(&mut t, a, "b");
    let f = file(&mut t, b, "f");
    assert_eq!(t.ancestors(f), vec![b, a, ROOT]);
    assert_eq!(t.ancestors(a), vec![ROOT]);
    assert_eq!(t.ancestors(ROOT), Vec::<Id>::new());
    // A file with a second name keeps its first directory.
    t.link(b, "f", ROOT, "g", 2).unwrap();
    assert_eq!(t.ancestors(f), vec![b, a, ROOT]);
}

#[test]
fn finds_names_case_insensitively() {
    let mut t = T::new(0o755, 0, 0);
    let a = mkdir(&mut t, ROOT, "Dir");
    let f = file(&mut t, a, "Straße");
    let second = file(&mut t, a, "dup");
    let third = file(&mut t, a, "DUP");
    assert_eq!(
        t.resolve_with(ROOT, "DIR/STRAẞE", false, true, false),
        Ok(Resolved::Node(f))
    );
    assert_eq!(t.resolve(ROOT, "DIR/STRAẞE", false), Err(Error::NotFound));
    // A sharp s has no simple folding to two letters.
    assert_eq!(
        t.resolve_with(ROOT, "dir/STRASSE", false, true, false),
        Ok(Resolved::Missing {
            parent: a,
            name: "STRASSE".to_string()
        })
    );
    assert_eq!(
        t.resolve_with(ROOT, "dir/straße", false, true, true),
        Err(Error::Exists)
    );
    // The first in listing order, unless one has exactly the name.
    assert_eq!(
        t.find(a, "Dup", true),
        Ok(Some(("dup".to_string(), second)))
    );
    assert_eq!(t.find(a, "DUP", true), Ok(Some(("DUP".to_string(), third))));
    assert_eq!(t.find(a, "Dup", false), Ok(None));
}
