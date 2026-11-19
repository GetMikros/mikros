//! The commands of paths and names, and find, as a system test: each
//! check runs one from where the system installs it, in a folder of its
//! own under `$TMPDIR`, and looks at its exit status and what it wrote.
//! It exits with 0 when every check passed, and otherwise with the
//! number, from 1, of the first that failed, after writing its name and
//! failure to standard error.

mod find;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PWD: &str = "/system/utilities/pwd";
const REALPATH: &str = "/system/utilities/realpath";
const READLINK: &str = "/system/utilities/readlink";
const BASENAME: &str = "/system/utilities/basename";
const DIRNAME: &str = "/system/utilities/dirname";
const MKTEMP: &str = "/system/utilities/mktemp";

/// What failed, and the exit status of the command, or -1 when it did
/// not exit.
pub struct Failure {
    what: &'static str,
    status: i32,
}

type Check = fn(&Folder) -> Result<(), Failure>;

/// What a run of a command gave.
pub struct Run {
    pub status: i32,
    pub out: String,
    pub err: String,
}

/// How a command is run besides its arguments.
#[derive(Default)]
pub struct With<'a> {
    /// Its current folder.
    pub folder: Option<&'a Path>,
    /// Environment variables to set, or with none to remove.
    pub env: &'a [(&'a str, Option<&'a str>)],
}

/// The folder of the checks, as made and as the C library resolves it.
pub struct Folder {
    pub path: PathBuf,
    pub resolved: PathBuf,
}

impl Folder {
    pub fn at(&self, name: &str) -> String {
        self.path.join(name).to_string_lossy().into_owned()
    }

    pub fn resolved(&self, name: &str) -> String {
        self.resolved.join(name).to_string_lossy().into_owned()
    }
}

fn main() {
    let checks: &[(&str, Check)] = &[
        ("pwd_writes_the_folder", pwd_writes_the_folder),
        ("pwd_bad_arguments", pwd_bad_arguments),
        ("realpath_resolves", realpath_resolves),
        (
            "realpath_names_what_is_missing",
            realpath_names_what_is_missing,
        ),
        ("readlink_reads_links", readlink_reads_links),
        ("readlink_refuses_files", readlink_refuses_files),
        ("basename_strips", basename_strips),
        ("basename_bad_arguments", basename_bad_arguments),
        ("dirname_strips", dirname_strips),
        ("dirname_bad_arguments", dirname_bad_arguments),
        (
            "mktemp_makes_files_and_folders",
            mktemp_makes_files_and_folders,
        ),
        ("mktemp_refuses", mktemp_refuses),
        ("find_names_and_types", find::names_and_types),
        ("find_sizes_and_empty", find::sizes_and_empty),
        ("find_exec", find::exec),
        ("find_json", find::json),
        ("find_delete", find::delete),
        ("find_unreadable_folder", find::unreadable_folder),
        ("find_bad_arguments", find::bad_arguments),
    ];
    let folder = match make_folder() {
        Ok(folder) => folder,
        Err(Failure { what, status }) => {
            eprintln!("making the folder FAILED: {what} ({status})");
            std::process::exit(checks.len() as i32 + 1);
        }
    };
    for (number, (name, check)) in checks.iter().enumerate() {
        if let Err(Failure { what, status }) = check(&folder) {
            eprintln!("{name} FAILED: {what} ({status})");
            let _ = std::fs::remove_dir_all(&folder.path);
            std::process::exit(number as i32 + 1);
        }
    }
    let _ = std::fs::remove_dir_all(&folder.path);
}

/// A new folder under `$TMPDIR`, or `/tmp`, with a file `a` of five
/// bytes, a folder `sub` and a link `l` to `a`.
fn make_folder() -> Result<Folder, Failure> {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let path = Path::new(&base).join(format!("files-paths-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(path.join("sub")).map_err(|_| failed("making the folder"))?;
    std::fs::write(path.join("a"), b"12345").map_err(|_| failed("writing a file"))?;
    std::os::unix::fs::symlink("a", path.join("l")).map_err(|_| failed("making a link"))?;
    let resolved = std::fs::canonicalize(&path).map_err(|_| failed("resolving the folder"))?;
    Ok(Folder { path, resolved })
}

pub fn failed(what: &'static str) -> Failure {
    Failure { what, status: -1 }
}

/// Runs `program` with `arguments` until it exits.
pub fn run(program: &str, arguments: &[&str]) -> Result<Run, Failure> {
    run_with(program, arguments, &With::default())
}

/// Runs `program` with `arguments` as `with` says, until it exits.
pub fn run_with(program: &str, arguments: &[&str], with: &With) -> Result<Run, Failure> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(folder) = with.folder {
        command.current_dir(folder);
    }
    for (name, value) in with.env {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    let output = command
        .output()
        .map_err(|_| failed("starting the command"))?;
    Ok(Run {
        status: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Passes when `ok` holds, and otherwise fails with `what` and the
/// status of `run`.
pub fn ensure(run: &Run, what: &'static str, ok: bool) -> Result<(), Failure> {
    match ok {
        true => Ok(()),
        false => {
            let _ = writeln!(std::io::stderr(), "out: {:?}\nerr: {:?}", run.out, run.err);
            Err(Failure {
                what,
                status: run.status,
            })
        }
    }
}

/// Passes when `run` succeeded, wrote `out` and nothing on standard
/// error.
pub fn wrote(run: &Run, what: &'static str, out: &str) -> Result<(), Failure> {
    ensure(
        run,
        what,
        run.status == 0 && run.out == out && run.err.is_empty(),
    )
}

/// Passes when `run` failed with `status` and an error that starts with
/// `start` and holds `holds`.
pub fn refused(
    run: &Run,
    what: &'static str,
    status: i32,
    start: &str,
    holds: &str,
) -> Result<(), Failure> {
    let ok = run.status == status && run.err.starts_with(start) && run.err.contains(holds);
    ensure(run, what, ok)
}

/// Without `$PWD`, pwd writes the folder resolved; with `$PWD` naming
/// it, `$PWD`, unless `-P` is given.
fn pwd_writes_the_folder(dir: &Folder) -> Result<(), Failure> {
    let sub = dir.path.join("sub");
    let given = sub.to_string_lossy().into_owned();
    let resolved = format!("{}\n", dir.resolved("sub"));
    let plain = With {
        folder: Some(&sub),
        env: &[("PWD", None)],
    };
    wrote(&run_with(PWD, &[], &plain)?, "the folder", &resolved)?;
    let logical = With {
        folder: Some(&sub),
        env: &[("PWD", Some(&given))],
    };
    let r = run_with(PWD, &[], &logical)?;
    wrote(&r, "$PWD", &format!("{given}\n"))?;
    let r = run_with(PWD, &["-L", "-P"], &logical)?;
    wrote(&r, "the folder with -P", &resolved)?;
    let stale = With {
        folder: Some(&sub),
        env: &[("PWD", Some("/system"))],
    };
    let r = run_with(PWD, &["-L"], &stale)?;
    wrote(&r, "not a $PWD of another folder", &resolved)
}

fn pwd_bad_arguments(_: &Folder) -> Result<(), Failure> {
    let r = run(PWD, &["-x"])?;
    refused(&r, "refusing -x", 2, "pwd: ", "unknown option -x")?;
    let r = run(PWD, &["extra"])?;
    refused(
        &r,
        "refusing an operand",
        2,
        "pwd: ",
        "unexpected argument extra",
    )
}

fn realpath_resolves(dir: &Folder) -> Result<(), Failure> {
    let r = run(REALPATH, &[&dir.at("l"), &dir.at("sub/../a")])?;
    let both = format!("{}\n{}\n", dir.resolved("a"), dir.resolved("a"));
    wrote(&r, "the link and the dots resolved", &both)?;
    let r = run(REALPATH, &["-m", &dir.at("no/x")])?;
    wrote(
        &r,
        "missing components with -m",
        &format!("{}\n", dir.resolved("no/x")),
    )?;
    let r = run(REALPATH, &["--relative-to", &dir.at("sub"), &dir.at("l")])?;
    wrote(&r, "a path relative to a folder", "../a\n")?;
    let r = run(REALPATH, &["-sz", &dir.at("sub/../l")])?;
    wrote(&r, "the link kept with -s", &format!("{}\0", dir.at("l")))
}

/// A path that cannot be resolved is named, the others still written,
/// and realpath exits with 1.
fn realpath_names_what_is_missing(dir: &Folder) -> Result<(), Failure> {
    let r = run(REALPATH, &[&dir.at("no/x"), &dir.at("a")])?;
    refused(&r, "naming the missing path", 1, "realpath: ", "no/x")?;
    ensure(
        &r,
        "still writing the other",
        r.out == format!("{}\n", dir.resolved("a")),
    )?;
    let r = run(REALPATH, &["-e", &dir.at("new")])?;
    refused(
        &r,
        "a missing last component with -e",
        1,
        "realpath: ",
        "new",
    )?;
    let r = run(REALPATH, &[])?;
    refused(
        &r,
        "refusing no operands",
        2,
        "realpath: ",
        "missing operand",
    )
}

fn readlink_reads_links(dir: &Folder) -> Result<(), Failure> {
    wrote(&run(READLINK, &[&dir.at("l")])?, "the target", "a\n")?;
    wrote(
        &run(READLINK, &["-n", &dir.at("l")])?,
        "no newline with -n",
        "a",
    )?;
    let r = run(READLINK, &["-f", &dir.at("l")])?;
    wrote(
        &r,
        "the target resolved with -f",
        &format!("{}\n", dir.resolved("a")),
    )
}

fn readlink_refuses_files(dir: &Folder) -> Result<(), Failure> {
    let r = run(READLINK, &[&dir.at("a"), &dir.at("l")])?;
    refused(&r, "naming a file", 1, "readlink: ", "not a link")?;
    ensure(&r, "still reading the link", r.out == "a\n")?;
    let r = run(READLINK, &["-e", &dir.at("no")])?;
    refused(&r, "a missing path with -e", 1, "readlink: ", "no")
}

fn basename_strips(_: &Folder) -> Result<(), Failure> {
    wrote(
        &run(BASENAME, &["/a/b.txt", ".txt"])?,
        "a name without a suffix",
        "b\n",
    )?;
    wrote(
        &run(BASENAME, &["//usr//lib//"])?,
        "a name with slashes",
        "lib\n",
    )?;
    wrote(&run(BASENAME, &["/"])?, "the root", "/\n")?;
    let r = run(BASENAME, &["-a", "-z", "x/y", "z/"])?;
    wrote(&r, "many names with NUL", "y\0z\0")?;
    wrote(
        &run(BASENAME, &["-s", ".c", "a.c", "b/.c"])?,
        "-s",
        "a\n.c\n",
    )
}

fn basename_bad_arguments(_: &Folder) -> Result<(), Failure> {
    let r = run(BASENAME, &[])?;
    refused(
        &r,
        "refusing no operands",
        2,
        "basename: ",
        "missing operand",
    )?;
    let r = run(BASENAME, &["a", "b", "c"])?;
    refused(
        &r,
        "refusing a third operand",
        2,
        "basename: ",
        "unexpected argument c",
    )
}

fn dirname_strips(_: &Folder) -> Result<(), Failure> {
    let r = run(DIRNAME, &["/a/b/", "a", "//a//b//", "/a", ""])?;
    wrote(&r, "the folders", "/a\n.\n//a\n/\n.\n")?;
    wrote(&run(DIRNAME, &["-z", "a/b"])?, "NUL with -z", "a\0")
}

fn dirname_bad_arguments(_: &Folder) -> Result<(), Failure> {
    let r = run(DIRNAME, &[])?;
    refused(
        &r,
        "refusing no operands",
        2,
        "dirname: ",
        "missing operand",
    )?;
    let r = run(DIRNAME, &["--nope", "a"])?;
    refused(
        &r,
        "refusing an unknown option",
        2,
        "dirname: ",
        "unknown option --nope",
    )
}

/// mktemp makes a file in `$TMPDIR`, which the process manager sets to
/// `/tmp` for every process, a folder in the folder of `-p`, with the
/// suffix given, and only a name with `-u`.
fn mktemp_makes_files_and_folders(dir: &Folder) -> Result<(), Failure> {
    let with = With::default();
    let r = run_with(MKTEMP, &[], &with)?;
    let path = r.out.trim_end();
    let start = "/tmp/tmp.";
    let named = path.starts_with(start) && path.len() == start.len() + 10;
    ensure(&r, "a name in $TMPDIR", r.status == 0 && named)?;
    let file = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0);
    let _ = std::fs::remove_file(path);
    ensure(&r, "an empty file", file)?;
    let r = run_with(
        MKTEMP,
        &["-d", "-p", &dir.at(""), "--suffix=.d", "x.XXXX"],
        &with,
    )?;
    let path = r.out.trim_end();
    let named = path.starts_with(&dir.at("x.")) && path.ends_with(".d");
    ensure(&r, "a folder of the template", r.status == 0 && named)?;
    let folder = std::fs::metadata(path).is_ok_and(|m| m.is_dir());
    ensure(&r, "a folder in the folder of -p", folder)?;
    let r = run_with(MKTEMP, &["-u", "y.XXX"], &with)?;
    let path = r.out.trim_end().to_string();
    ensure(
        &r,
        "a name with -u",
        r.status == 0 && path.starts_with("/tmp/y."),
    )?;
    let made = std::fs::symlink_metadata(&path).is_ok();
    ensure(&r, "nothing made with -u", !made)
}

fn mktemp_refuses(dir: &Folder) -> Result<(), Failure> {
    let r = run(MKTEMP, &["-p", &dir.at(""), "aXX"])?;
    refused(&r, "a template of two X", 2, "mktemp: ", "at least 3 X")?;
    let r = run(MKTEMP, &["-p", &dir.at("no"), "aXXX"])?;
    refused(
        &r,
        "a folder that is not there",
        1,
        "mktemp: cannot make a file",
        "no/aXXX",
    )?;
    let r = run(MKTEMP, &["-q", "-p", &dir.at("no"), "aXXX"])?;
    ensure(&r, "quiet with -q", r.status == 1 && r.err.is_empty())
}
