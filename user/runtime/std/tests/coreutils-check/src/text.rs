//! The utilities that read and write text, and those that compute: each
//! with input or files that it makes, and an error, mostly a missing
//! file or a bad argument.

use crate::{Checks, first_bytes};

pub fn check(c: &mut Checks) {
    let dir = c.dir;
    let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).expect(name);
    write("s1", "a\nb\n");
    write("s2", "b\nc\n");
    write("j1", "1 x\n2 y\n");
    write("j2", "1 p\n2 q\n");

    c.ok(&["base32"], "hi", "NBUQ====\n");
    c.err_with(&["base32", "-d"], "!!", 1);
    c.ok(&["base64"], "hi", "aGk=\n");
    c.err_with(&["base64", "-d"], "!!", 1);
    c.ok(&["basenc", "--base64"], "hi", "aGk=\n");
    c.err(&["basenc"], 1);
    c.ok(&["comm", "s1", "s2"], "", "a\n\t\tb\n\tc\n");
    c.err(&["comm", "s1", "missing"], 1);
    c.ok(&["join", "j1", "j2"], "", "1 x p\n2 y q\n");
    c.err(&["join", "j1", "missing"], 1);
    c.ok(&["paste", "s1", "s2"], "", "a\tb\nb\tc\n");
    c.err(&["paste", "missing"], 1);
    c.ok(&["expand"], "a\tb\n", "a       b\n");
    c.err(&["expand", "missing"], 1);
    c.ok(&["unexpand", "-a"], "        a\n", "\ta\n");
    c.err(&["unexpand", "missing"], 1);
    c.ok(&["fmt"], "a\nb\n", "a b\n");
    c.err(&["fmt", "missing"], 1);
    c.ok(&["fold", "-w", "2"], "abcd\n", "ab\ncd\n");
    c.err(&["fold", "missing"], 1);
    c.ok(&["nl"], "a\n", "     1\ta\n");
    c.err(&["nl", "missing"], 1);
    c.ok(&["od", "-c"], "ab", "0000000   a   b\n0000002\n");
    c.err(&["od", "missing"], 1);
    c.ok(&["pr", "-t"], "a\n", "a\n");
    c.err(&["pr", "missing"], 1);
    c.ok_if(
        &["ptx"],
        "a b\n",
        |o| o.contains("a b"),
        "an index of \"a b\"",
    );
    c.err(&["ptx", "missing"], 1);
    c.ok(&["tac", "a.txt"], "", "three\ntwo\none\n");
    c.err(&["tac", "missing"], 1);
    c.ok(&["tsort"], "a b\n", "a\nb\n");
    c.err_with(&["tsort"], "a\n", 1);
    c.ok(&["csplit", "a.txt", "2"], "", "4\n10\n");
    let piece = std::fs::read_to_string(dir.join("xx01"));
    c.holds(
        &["csplit", "a.txt", "2"],
        piece.is_ok_and(|p| p == "two\nthree\n"),
        "xx01 holds the lines from 2 on",
    );
    c.err(&["csplit", "missing", "2"], 1);
    c.ok(&["split", "-l", "1", "a.txt"], "", "");
    let piece = std::fs::read_to_string(dir.join("xab"));
    c.holds(
        &["split", "-l", "1", "a.txt"],
        piece.is_ok_and(|p| p == "two\n"),
        "xab holds the second line",
    );
    c.err(&["split", "missing"], 1);

    c.ok(&["numfmt", "--to=si", "1000"], "", "1.0k\n");
    c.err(&["numfmt", "x"], 2);
    c.ok(&["seq", "3"], "", "1\n2\n3\n");
    c.err(&["seq", "x"], 1);
    c.ok(&["printf", "%s-%d", "a", "3"], "", "a-3");
    c.err(&["printf", "%d", "x"], 1);
    c.ok(&["expr", "1", "+", "2"], "", "3\n");
    c.err(&["expr", "1", "+"], 2);
    c.ok(&["factor", "12"], "", "12: 2 2 3\n");
    c.err(&["factor", "x"], 1);
    c.ok(&["shuf", "-i", "1-1"], "", "1\n");
    c.err(&["shuf", "-i", "x"], 1);
    c.ok(&["test", "-f", "a.txt"], "", "");
    c.err(&["test", "1", "-eq", "x"], 2);
    // `yes` writes until it is stopped, and fails only on a write error.
    let yes = first_bytes(dir, &["yes"], 4);
    c.holds(
        &["yes"],
        yes.as_deref() == Some(b"y\ny\n"),
        "yes writes y lines",
    );
}
