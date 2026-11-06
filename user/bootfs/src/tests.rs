use super::*;
use alloc::vec;

const BLOCK: usize = 512;

/// A ustar member named `name` with type `flag`, `data` and `link`.
fn member(name: &str, flag: u8, data: &[u8], link: &str) -> Vec<u8> {
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000007\0");
    h[124..136].copy_from_slice(alloc::format!("{:011o}\0", data.len()).as_bytes());
    h[136..148].copy_from_slice(b"00000000011\0");
    h[156] = flag;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    let mut out = h.to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(BLOCK), 0);
    out
}

fn tree(members: &[Vec<u8>]) -> Tree {
    let mut image: Vec<u8> = members.concat();
    image.extend_from_slice(&[0; 2 * BLOCK]);
    Tree::new(&image)
}

fn sample() -> Tree {
    tree(&[
        member("usr/bin/sh", b'0', b"shell", ""),
        member("usr/bin/", b'5', b"", ""),
        member("usr/bin/ash", b'1', b"", "usr/bin/sh"),
        member("usr/lib/up", b'2', b"", "../bin"),
        member("usr/lib/out", b'2', b"", "../../../etc"),
        member("usr/lib/abs", b'2', b"", "/usr/bin"),
        member("usr/lib/self", b'2', b"", "self"),
        member("dev/null", b'3', b"", ""),
    ])
}

fn node(t: &Tree, dir: usize, path: &str) -> usize {
    match t.resolve(dir, path.as_bytes(), false) {
        Ok(Resolved::Node(n)) => n,
        other => panic!("{path}: {other:?}"),
    }
}

#[test]
fn builds_files_directories_and_links() {
    let t = sample();
    let sh = node(&t, 0, "usr/bin/sh");
    assert_eq!(
        t.nodes[sh].kind,
        NodeKind::File {
            offset: BLOCK,
            size: 5
        }
    );
    assert_eq!(
        (t.nodes[sh].mode, t.nodes[sh].owner, t.nodes[sh].mtime),
        (0o644, 7, 9)
    );
    assert_eq!(node(&t, 0, "usr/bin/ash"), sh);
    assert_eq!(t.nodes[sh].links, 2);
    let bin = node(&t, 0, "usr/bin");
    assert_eq!(
        t.nodes[bin].mode, 0o644,
        "the directory member sets attributes"
    );
    let usr = node(&t, 0, "usr");
    assert_eq!((t.nodes[usr].mode, t.nodes[usr].mtime), (0o755, 0));
    assert_eq!(t.skipped, [(b"dev/null".to_vec(), b'3')]);
}

#[test]
fn skips_hard_links_to_missing_members() {
    let t = tree(&[member("a", b'1', b"", "missing")]);
    assert_eq!(t.skipped, [(b"a".to_vec(), b'1')]);
    assert_eq!(t.child(0, b"a"), None);
}

#[test]
fn rejects_invalid_paths() {
    let t = sample();
    let long = "x".repeat(NAME_MAX + 1);
    for (path, error) in [
        ("", Error::Invalid),
        ("/usr", Error::Invalid),
        ("usr//bin", Error::Invalid),
        ("usr/../usr", Error::AccessDenied),
        (long.as_str(), Error::NameTooLong),
        ("usr/missing", Error::NotFound),
        ("usr/bin/sh/x", Error::NotDirectory),
    ] {
        assert_eq!(t.resolve(0, path.as_bytes(), false), Err(error), "{path}");
    }
    assert_eq!(node(&t, 0, "usr/bin/"), node(&t, 0, "usr/bin"));
}

#[test]
fn follows_links_that_stay_within_the_directory() {
    let t = sample();
    let bin = node(&t, 0, "usr/bin");
    assert_eq!(node(&t, 0, "usr/lib/up"), bin);
    assert_eq!(node(&t, 0, "usr/lib/up/sh"), node(&t, 0, "usr/bin/sh"));
    let usr = node(&t, 0, "usr");
    assert_eq!(node(&t, usr, "lib/up/sh"), node(&t, 0, "usr/bin/sh"));
}

#[test]
fn redirects_links_that_leave_the_directory() {
    let t = sample();
    let redirect = |dir, path: &str| t.resolve(dir, path.as_bytes(), false);
    assert_eq!(
        redirect(0, "usr/lib/out/passwd"),
        Ok(Resolved::Redirect(b"../../../etc/passwd".to_vec()))
    );
    assert_eq!(
        redirect(0, "usr/lib/abs/sh"),
        Ok(Resolved::Redirect(b"/usr/bin/sh".to_vec()))
    );
    let lib = node(&t, 0, "usr/lib");
    assert_eq!(
        redirect(lib, "up/sh"),
        Ok(Resolved::Redirect(b"../bin/sh".to_vec()))
    );
}

#[test]
fn opens_links_themselves_with_no_follow() {
    let t = sample();
    let up = t.resolve(0, b"usr/lib/up", true);
    let Ok(Resolved::Node(up)) = up else {
        panic!("{up:?}")
    };
    assert_eq!(t.nodes[up].kind, NodeKind::Symlink(b"../bin".to_vec()));
}

#[test]
fn stops_after_too_many_links() {
    let t = sample();
    assert_eq!(t.resolve(0, b"usr/lib/self", false), Err(Error::Loop));
}

#[test]
fn lists_directories_in_name_order() {
    let t = sample();
    let lib = node(&t, 0, "usr/lib");
    let (first, next) = t.read_directory(lib, 0, 3);
    let names: Vec<&[u8]> = first.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, [&b"abs"[..], b"out", b"self"]);
    let (rest, end) = t.read_directory(lib, next, 3);
    assert_eq!(rest.len(), 1);
    assert_eq!(t.read_directory(lib, end, 3).0, vec![]);
}

#[test]
fn finds_what_a_directory_contains() {
    let t = sample();
    let usr = node(&t, 0, "usr");
    assert!(t.contains(usr, node(&t, 0, "usr/bin/sh")));
    assert!(t.contains(usr, usr));
    assert!(!t.contains(usr, 0));
    // A link is followed by resolve, not by contains: the link up resolves
    // to usr/bin, which usr contains, but usr/bin does not contain usr.
    let bin = node(&t, 0, "usr/lib/up");
    assert!(t.contains(usr, bin) && !t.contains(bin, usr));
}
