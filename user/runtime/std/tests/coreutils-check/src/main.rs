//! A program that checks the command-line utilities of uutils coreutils
//! on Mikros. Given a writable folder, it makes `coreutils-check` in it,
//! runs each utility there against files it creates, and checks what
//! each writes and how it exits, for one ordinary use and, except for
//! `echo` and `true`, one error. It
//! prints each check that failed, removes its folder, and exits with 0
//! and a greeting when all passed, and with the number that failed
//! otherwise.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// What a run wrote and how it ended.
struct Ran {
    out: String,
    err: String,
    code: Option<i32>,
}

/// Runs `args` in `dir` with `input` on standard input.
fn run(dir: &Path, args: &[&str], input: &str) -> Ran {
    let child = Command::new(args[0])
        .args(&args[1..])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            return Ran {
                out: String::new(),
                err: format!("not started: {e}"),
                code: None,
            };
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    match child.wait_with_output() {
        Ok(o) => Ran {
            out: String::from_utf8_lossy(&o.stdout).into(),
            err: String::from_utf8_lossy(&o.stderr).into(),
            code: o.status.code(),
        },
        Err(e) => Ran {
            out: String::new(),
            err: format!("not waited for: {e}"),
            code: None,
        },
    }
}

/// The checks, which count their failures.
struct Checks<'a> {
    dir: &'a Path,
    failed: usize,
}

impl Checks<'_> {
    /// Checks that `args` with `input` writes `out` and exits with 0.
    fn ok(&mut self, args: &[&str], input: &str, out: &str) {
        let r = run(self.dir, args, input);
        if r.code != Some(0) || r.out != out {
            self.fail(args, &r, &format!("{out:?} and status 0"));
        }
    }

    /// Checks that `args` fails with status `code` and an error message.
    fn err(&mut self, args: &[&str], code: i32) {
        let r = run(self.dir, args, "");
        if r.code != Some(code) || r.err.is_empty() {
            self.fail(args, &r, &format!("an error and status {code}"));
        }
    }

    /// Checks `cond`, which `args` should have made true.
    fn holds(&mut self, args: &[&str], cond: bool, what: &str) {
        if !cond {
            println!("coreutils-check: {args:?}: {what} does not hold");
            self.failed += 1;
        }
    }

    fn fail(&mut self, args: &[&str], r: &Ran, wanted: &str) {
        println!(
            "coreutils-check: {args:?} wrote {:?} and {:?} and ended with {:?}, not {wanted}",
            r.out, r.err, r.code
        );
        self.failed += 1;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [base] = args.as_slice() else {
        eprintln!("usage: coreutils-check FOLDER");
        std::process::exit(100);
    };
    let dir = Path::new(base).join("coreutils-check");
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir(&dir) {
        eprintln!("coreutils-check: making {}: {e}", dir.display());
        std::process::exit(100);
    }
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").expect("a.txt");
    let mut c = Checks {
        dir: &dir,
        failed: 0,
    };
    let at = |name: &str| dir.join(name);

    // `echo` and `true` have no error to check; `false`'s ordinary use is
    // its failure.
    c.ok(&["echo", "hello", "world"], "", "hello world\n");
    c.ok(&["cat", "a.txt"], "", "one\ntwo\nthree\n");
    c.err(&["cat", "missing"], 1);
    c.ok(&["ls", "a.txt"], "", "a.txt\n");
    c.err(&["ls", "missing"], 2);
    c.ok(&["mkdir", "d"], "", "");
    c.holds(&["mkdir", "d"], at("d").is_dir(), "d is a folder");
    c.err(&["mkdir", "d"], 1);
    c.ok(&["rmdir", "d"], "", "");
    c.holds(&["rmdir", "d"], !at("d").exists(), "d is gone");
    c.err(&["rmdir", "missing"], 1);
    c.ok(&["touch", "t"], "", "");
    c.holds(&["touch", "t"], at("t").is_file(), "t is a file");
    c.err(&["touch", "missing/t"], 1);
    c.ok(&["cp", "a.txt", "b.txt"], "", "");
    c.holds(
        &["cp", "a.txt", "b.txt"],
        std::fs::read_to_string(at("b.txt")).is_ok_and(|s| s == "one\ntwo\nthree\n"),
        "b.txt is a copy of a.txt",
    );
    c.err(&["cp", "missing", "x"], 1);
    c.ok(&["mv", "b.txt", "c.txt"], "", "");
    c.holds(
        &["mv", "b.txt", "c.txt"],
        !at("b.txt").exists() && at("c.txt").is_file(),
        "b.txt is now c.txt",
    );
    c.err(&["mv", "missing", "x"], 1);
    c.ok(&["rm", "c.txt"], "", "");
    c.holds(&["rm", "c.txt"], !at("c.txt").exists(), "c.txt is gone");
    c.err(&["rm", "missing"], 1);
    c.ok(&["ln", "-s", "a.txt", "s"], "", "");
    c.holds(
        &["ln", "-s", "a.txt", "s"],
        std::fs::read_link(at("s")).is_ok_and(|t| t == Path::new("a.txt")),
        "s links to a.txt",
    );
    c.err(&["ln", "-s", "a.txt", "s"], 1);
    c.ok(&["pwd"], "", &format!("{}\n", dir.display()));
    c.err(&["pwd", "--no-such-option"], 1);
    c.ok(&["head", "-n", "1", "a.txt"], "", "one\n");
    c.err(&["head", "missing"], 1);
    c.ok(&["tail", "-n", "1", "a.txt"], "", "three\n");
    c.err(&["tail", "missing"], 1);
    c.ok(&["wc", "-l", "a.txt"], "", "3 a.txt\n");
    c.err(&["wc", "missing"], 1);
    c.ok(&["sort"], "b\na\n", "a\nb\n");
    c.err(&["sort", "missing"], 2);
    c.ok(&["uniq"], "a\na\nb\n", "a\nb\n");
    c.err(&["uniq", "missing"], 1);
    c.ok(&["cut", "-d", ",", "-f", "2"], "x,y\n", "y\n");
    c.err(&["cut"], 1);
    c.ok(&["tr", "a-z", "A-Z"], "abc", "ABC");
    c.err(&["tr"], 1);
    c.ok(&["tee", "t2"], "x\n", "x\n");
    c.holds(
        &["tee", "t2"],
        std::fs::read_to_string(at("t2")).is_ok_and(|s| s == "x\n"),
        "t2 holds what tee read",
    );
    c.err(&["tee", "missing/t"], 1);
    c.ok(&["date", "-u", "-d", "@0", "+%Y"], "", "1970\n");
    c.err(&["date", "-d", "not a date"], 1);
    c.ok(&["env", "-i", "A=1"], "", "A=1\n");
    c.err(&["env", "-i", "no-such-program"], 127);
    c.ok(&["sleep", "0"], "", "");
    c.err(&["sleep", "x"], 1);
    c.ok(&["true"], "", "");
    let r = run(&dir, &["false"], "");
    c.holds(&["false"], r.code == Some(1), "false exits with 1");
    c.ok(&["basename", "/a/b.txt", ".txt"], "", "b\n");
    c.err(&["basename"], 1);
    c.ok(&["dirname", "/a/b"], "", "/a\n");
    c.err(&["dirname"], 1);
    c.ok(
        &["realpath", "a.txt"],
        "",
        &format!("{}\n", at("a.txt").display()),
    );
    c.err(&["realpath", "-e", "missing"], 1);
    c.ok(&["stat", "-c", "%s", "a.txt"], "", "14\n");
    c.err(&["stat", "missing"], 1);
    c.ok(&["chmod", "600", "a.txt"], "", "");
    c.ok(&["stat", "-c", "%a", "a.txt"], "", "600\n");
    c.err(&["chmod", "600", "missing"], 1);
    let du = run(&dir, &["du", "-s", "a.txt"], "");
    c.holds(
        &["du", "-s", "a.txt"],
        du.code == Some(0) && du.out.ends_with("\ta.txt\n"),
        &format!("du reports a.txt ({:?}, {:?})", du.out, du.err),
    );
    c.err(&["du", "missing"], 1);
    // df lists the namespace's entries, and a path's entry for a path.
    let df = run(&dir, &["df", "."], "");
    c.holds(
        &["df", "."],
        df.code == Some(0) && df.out.lines().count() == 2 && df.out.ends_with(" /Users/ada\n"),
        &format!(
            "df reports the entry /Users/ada ({:?}, {:?})",
            df.out, df.err
        ),
    );
    let df = run(&dir, &["df"], "");
    let mounts: Vec<&str> = df
        .out
        .lines()
        .filter_map(|l| l.split(' ').next_back())
        .collect();
    c.holds(
        &["df"],
        df.code == Some(0)
            && ["/usr", "/tmp", "/Users/ada"]
                .iter()
                .all(|m| mounts.contains(m)),
        &format!(
            "df lists the namespace's entries ({:?}, {:?})",
            df.out, df.err
        ),
    );
    c.err(&["df", "missing"], 1);

    let _ = std::fs::remove_dir_all(&dir);
    if c.failed > 0 {
        println!("coreutils-check: {} checks failed", c.failed);
        std::process::exit(c.failed.min(99) as i32);
    }
    println!("coreutils, world");
}
