//! The utilities that make, change and look at files, beyond those of
//! the main checks: each on files it makes, and an error, mostly a
//! missing file.

use crate::Checks;
use std::fs;
use std::os::unix::fs::MetadataExt;

pub fn check(c: &mut Checks) {
    let dir = c.dir;
    let at = |name: &str| dir.join(name);
    let meta = fs::metadata(at("a.txt")).expect("a.txt");
    let (uid, gid) = (meta.uid().to_string(), meta.gid().to_string());

    // Owners only stay as they are.
    c.ok(&["chown", &uid, "a.txt"], "", "");
    c.err(&["chown", &uid, "missing"], 1);
    c.ok(&["chgrp", &gid, "a.txt"], "", "");
    c.err(&["chgrp", &gid, "missing"], 1);
    c.ok(&["dd", "if=a.txt", "status=none"], "", "one\ntwo\nthree\n");
    c.err(&["dd", "if=missing"], 1);
    c.ok(&["dir", "a.txt"], "", "a.txt\n");
    c.err(&["dir", "missing"], 2);
    c.ok_if(
        &["vdir", "a.txt"],
        "",
        |o| o.starts_with('-') && o.ends_with(" a.txt\n"),
        "a long listing of a.txt",
    );
    c.err(&["vdir", "missing"], 2);
    c.ok(&["install", "a.txt", "i.txt"], "", "");
    c.holds(
        &["install", "a.txt", "i.txt"],
        fs::read_to_string(at("i.txt")).is_ok_and(|s| s == "one\ntwo\nthree\n"),
        "i.txt is a copy of a.txt",
    );
    c.err(&["install", "missing", "x"], 1);
    c.ok(&["link", "a.txt", "l.txt"], "", "");
    c.holds(
        &["link", "a.txt", "l.txt"],
        fs::metadata(at("a.txt")).is_ok_and(|m| m.nlink() == 2),
        "a.txt has two links",
    );
    c.err(&["link", "missing", "x"], 1);
    c.ok(&["unlink", "l.txt"], "", "");
    c.holds(&["unlink", "l.txt"], !at("l.txt").exists(), "l.txt is gone");
    c.err(&["unlink", "missing"], 1);
    c.ok_if(
        &["mktemp", "-p", "."],
        "",
        |o| dir.join(o.trim_end()).is_file(),
        "the name of a new file",
    );
    c.err(&["mktemp", "-p", "missing"], 1);
    // `s` links to a.txt since the main checks; readlink fails silently.
    c.ok(&["readlink", "s"], "", "a.txt\n");
    c.fails(&["readlink", "a.txt"], 1, "");
    fs::write(at("sh.txt"), "secret").expect("sh.txt");
    // Exactly the file's size, which shred otherwise rounds up to a
    // block.
    c.ok(&["shred", "-x", "sh.txt"], "", "");
    c.holds(
        &["shred", "-x", "sh.txt"],
        fs::read(at("sh.txt")).is_ok_and(|s| s.len() == 6 && s != b"secret"),
        "sh.txt is overwritten",
    );
    c.err(&["shred", "missing"], 1);
    c.ok(&["sync"], "", "");
    c.err(&["sync", "missing"], 1);
    c.ok(&["truncate", "-s", "2", "tr.txt"], "", "");
    c.holds(
        &["truncate", "-s", "2", "tr.txt"],
        fs::metadata(at("tr.txt")).is_ok_and(|m| m.len() == 2),
        "tr.txt has 2 bytes",
    );
    c.err(&["truncate", "tr.txt"], 1);
    c.ok(&["pathchk", "a"], "", "");
    c.err(&["pathchk", ""], 1);
    c.ok_if(
        &["dircolors", "-b"],
        "",
        |o| o.starts_with("LS_COLORS="),
        "LS_COLORS for the shell",
    );
    c.err(&["dircolors", "missing"], 1);
    // Without a terminal, more writes the file as it is. A missing file
    // is an error that leaves its status 0.
    c.ok(&["more", "a.txt"], "", "one\ntwo\nthree\n");
    c.err(&["more", "missing"], 0);
}
