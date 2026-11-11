//! The utilities about the system, the user, processes and terminals:
//! each in an ordinary use, and an error, mostly a bad argument.

use crate::{Checks, VARIABLE, run_with};

fn digits(text: &str) -> bool {
    text.strip_suffix('\n')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

pub fn check(c: &mut Checks) {
    c.ok(&["arch"], "", "x86_64\n");
    c.err(&["arch", "x"], 1);
    c.ok(&["uname", "-m"], "", "x86_64\n");
    c.err(&["uname", "-x"], 1);
    c.ok_if(&["nproc"], "", digits, "a number");
    c.err(&["nproc", "x"], 1);
    c.ok(&["hostname"], "", "box\n");
    c.err(&["hostname", "-x"], 1);
    c.ok_if(
        &["hostid"],
        "",
        |o| o.len() == 9 && o.bytes().take(8).all(|b| b.is_ascii_hexdigit()),
        "eight hexadecimal digits",
    );
    c.err(&["hostid", "x"], 1);
    c.ok_if(&["id", "-u"], "", digits, "a user ID");
    c.err(&["id", "nosuchuser"], 1);
    c.ok(&["groups"], "", "ada admin\n");
    c.err(&["groups", "nosuchuser"], 1);
    c.ok(&["whoami"], "", "ada\n");
    c.err(&["whoami", "x"], 1);
    c.ok(&["logname"], "", "ada\n");
    c.err(&["logname", "x"], 1);
    c.ok(&["users"], "", "");
    c.err(&["users", "a", "b"], 1);
    c.ok(&["who"], "", "");
    c.err(&["who", "-x"], 1);
    c.ok_if(&["pinky"], "", |o| o.starts_with("Login"), "a heading");
    c.err(&["pinky", "-x"], 1);
    c.ok_if(&["uptime"], "", |o| o.contains(" up "), "the time up");
    c.err(&["uptime", "-x"], 1);
    c.ok_if(&["kill", "-l"], "", |o| o.contains("HUP"), "the signals");
    c.err(&["kill", "-s", "NOPE", "1"], 1);
    c.ok(&["nice"], "", "0\n");
    c.err(&["nice", "-n", "x", "true"], 125);
    c.ok(&["nohup", "true"], "", "");
    c.err(&["nohup", "no-such-program"], 127);
    c.ok(&["timeout", "5", "true"], "", "");
    c.err(&["timeout", "x", "true"], 125);
    c.ok(&["printenv", VARIABLE.0], "", &format!("{}\n", VARIABLE.1));
    c.err(&["printenv", "--no-such-option"], 2);

    // With this program's terminal, and then without one.
    let tty = run_with(c.dir, &["tty"], None);
    c.holds(
        &["tty"],
        tty.code == Some(0) && tty.out.starts_with("/dev/pts/") && digits(&tty.out[9..]),
        &format!("tty names the terminal ({:?}, {:?})", tty.out, tty.err),
    );
    c.fails(&["tty"], 1, "not a tty\n");
    let size = run_with(c.dir, &["stty", "size"], None);
    let rows_cols = size
        .out
        .strip_suffix('\n')
        .and_then(|s| s.split_once(' '))
        .is_some_and(|(r, k)| digits(&format!("{r}\n")) && digits(&format!("{k}\n")));
    c.holds(
        &["stty", "size"],
        size.code == Some(0) && rows_cols,
        &format!("stty gives the size ({:?}, {:?})", size.out, size.err),
    );
    c.err(&["stty", "size"], 1);
}
