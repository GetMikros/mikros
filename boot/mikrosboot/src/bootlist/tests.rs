use super::*;
use crate::btrfs::tests::{mkfs, write};
use std::os::unix::fs::symlink;
use std::path::Path;

fn volume(fill: impl FnOnce(&Path)) -> (Volume<Vec<u8>>, Node) {
    let image = mkfs("bootlist", &["--subvol", "version"], |t| {
        fill(&t.join("version"))
    });
    let mut v = Volume::open(image).unwrap();
    let top = v.top().unwrap();
    let root = v.lookup(top, b"version").unwrap().unwrap();
    (v, root)
}

fn paths(modules: &[Module]) -> Vec<&str> {
    modules.iter().map(|m| m.path.as_str()).collect()
}

#[test]
fn reads_lines() {
    let list = b"# comment\n\n  init\t\nbin/\n lib/a/b \n#x/\n";
    let lines = parse(list).unwrap();
    assert_eq!(
        lines,
        [
            Line {
                path: "init",
                directory: false
            },
            Line {
                path: "bin",
                directory: true
            },
            Line {
                path: "lib/a/b",
                directory: false
            },
        ]
    );
}

#[test]
fn refuses_invalid_lines() {
    for (list, line) in [
        (&b"init\n/abs\n"[..], 2),
        (b"a//b", 1),
        (b"./a", 1),
        (b"a/..", 1),
        (b"a//", 1),
        (b"/", 1),
        (b"ok\n\nx/./y\n", 3),
    ] {
        assert_eq!(parse(list), Err(Error::Line(line)), "{list:?}");
    }
    let long = "a/".repeat(512) + "b";
    assert_eq!(parse(long.as_bytes()), Err(Error::Line(1)));
    assert_eq!(parse(&long.as_bytes()[2..]).unwrap().len(), 1);
    assert_eq!(parse(b"bad \xff"), Err(Error::NotUtf8));
}

#[test]
fn loads_files_in_list_and_byte_order() {
    let (mut v, root) = volume(|t| {
        write(t, "init", b"init");
        write(t, "lib/a/x", b"x");
        write(t, "lib/a-b", b"ab");
        write(t, "lib/B", b"B");
        write(t, "lib/deep/er/f", b"f");
        write(t, "bin/tool", b"tool");
    });
    let list = b"init\nlib/\nbin/tool\nlib/a/x\ninit\n";
    let modules = modules(&mut v, root, list).unwrap();
    assert_eq!(
        paths(&modules),
        [
            "init",
            "lib/B",
            "lib/a-b",
            "lib/a/x",
            "lib/deep/er/f",
            "bin/tool"
        ]
    );
    assert_eq!(modules[2].size, 2);
}

#[test]
fn follows_links() {
    let (mut v, root) = volume(|t| {
        write(t, "real/file", b"file");
        write(t, "real/sub/inner", b"inner");
        symlink("real/file", t.join("top")).unwrap();
        std::fs::create_dir_all(t.join("dir")).unwrap();
        symlink("../real/file", t.join("dir/to-file")).unwrap();
        symlink("../real/sub", t.join("dir/to-dir")).unwrap();
        symlink("to-file", t.join("dir/chain")).unwrap();
        symlink("real", t.join("realdir")).unwrap();
    });
    let list = b"top\ndir/\nrealdir/sub/inner\n";
    let modules = modules(&mut v, root, list).unwrap();
    assert_eq!(
        paths(&modules),
        [
            "top",
            "dir/chain",
            "dir/to-dir/inner",
            "dir/to-file",
            "realdir/sub/inner"
        ]
    );
    let file = &modules[0];
    assert_eq!(file.node, modules[1].node);
    assert_eq!(file.node, modules[3].node);
    assert_eq!(file.size, 4);
}

#[test]
fn refuses_links_that_leave_the_version() {
    let (mut v, root) = volume(|t| {
        write(t, "file", b"f");
        symlink("/file", t.join("absolute")).unwrap();
        symlink("../version/file", t.join("outside")).unwrap();
        symlink("loop", t.join("loop")).unwrap();
        symlink("missing", t.join("dangling")).unwrap();
    });
    let mut err = |list: &[u8]| modules(&mut v, root, list).unwrap_err();
    assert_eq!(err(b"absolute"), Error::AbsoluteLink("absolute".into()));
    assert_eq!(err(b"outside"), Error::Outside("outside".into()));
    assert_eq!(err(b"loop"), Error::Links("loop".into()));
    assert_eq!(err(b"dangling"), Error::Missing("dangling".into()));
    drop(err);
    // A directory that holds a link to its parent is refused once the
    // paths below it lead through too many links.
    let (mut v, root) = volume(|t| {
        write(t, "cycle/file", b"f");
        symlink("..", t.join("cycle/up")).unwrap();
    });
    assert!(matches!(
        modules(&mut v, root, b"cycle/"),
        Err(Error::Links(_))
    ));
}

#[test]
fn checks_what_lines_name() {
    let (mut v, root) = volume(|t| {
        write(t, "file", b"f");
        write(t, "dir/inner", b"i");
    });
    let mut err = |list: &[u8]| modules(&mut v, root, list).unwrap_err();
    assert_eq!(err(b"missing"), Error::Missing("missing".into()));
    assert_eq!(err(b"dir"), Error::NotAFile("dir".into()));
    assert_eq!(err(b"file/"), Error::NotADirectory("file/".into()));
    assert_eq!(err(b"file/x"), Error::NotADirectory("file/x".into()));
    drop(err);
    let found = file(&mut v, root, "dir/inner").unwrap();
    assert_eq!(found.size, 1);
    assert_eq!(
        file(&mut v, root, "dir"),
        Err(Error::NotAFile("dir".into()))
    );
}

#[test]
fn limits_the_number_of_modules() {
    let (mut v, root) = volume(|t| {
        for i in 0..MODULES_MAX {
            write(t, &format!("many/{i:04}"), b"");
        }
        write(t, "one-more", b"");
    });
    let all = modules(&mut v, root, b"many/\nmany/0001\n").unwrap();
    assert_eq!(all.len(), MODULES_MAX);
    assert_eq!(all[0].size, 0);
    assert_eq!(
        modules(&mut v, root, b"many/\none-more"),
        Err(Error::TooMany)
    );
}
