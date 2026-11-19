//! The checks of printf, seq, base64, sha256sum and md5sum.

use crate::{Check, Failure, ensure, file, path, refused, run, run_with, utility, wrote};

pub fn checks() -> Vec<(&'static str, Check)> {
    vec![
        ("printf_reuses_its_format", printf_reuses_its_format),
        ("printf_names_a_bad_number", printf_names_a_bad_number),
        ("seq_counts", seq_counts),
        ("seq_refuses_a_bad_number", seq_refuses_a_bad_number),
        ("base64_encodes_and_decodes", base64_encodes_and_decodes),
        ("base64_refuses_bad_input", base64_refuses_bad_input),
        ("sha256sum_hashes_and_checks", sha256sum_hashes_and_checks),
        (
            "sha256sum_names_a_missing_file",
            sha256sum_names_a_missing_file,
        ),
        ("md5sum_hashes_and_checks", md5sum_hashes_and_checks),
        ("md5sum_names_a_missing_file", md5sum_names_a_missing_file),
    ]
}

/// The format is written again for the remaining arguments, and an
/// argument that starts with `-` is not an option.
fn printf_reuses_its_format() -> Result<(), Failure> {
    let printf = utility("printf");
    let arguments = ["%s=%d %5.2f\\n", "a", "1", "3.14159", "b", "-2", "1"];
    let run = run(&printf, &arguments)?;
    wrote(&run, "each pair on a line", "a=1  3.14\nb=-2  1.00\n")?;
    let run = run_with(&printf, &["%b|%q", "x\\ty", "a b"], None)?;
    wrote(&run, "%b and %q", "x\ty|'a b'")
}

/// An argument that is not a number is named, written as 0, and printf
/// exits with 1.
fn printf_names_a_bad_number() -> Result<(), Failure> {
    let run = run(&utility("printf"), &["%d\\n", "abc"])?;
    refused(&run, "failing", "printf", 1)?;
    ensure(&run, "naming the argument", run.err.contains("'abc'"))?;
    ensure(&run, "writing 0", run.out == "0\n")
}

fn seq_counts() -> Result<(), Failure> {
    let seq = utility("seq");
    let run = run(&seq, &["-s", ",", "-3", "2", "3"])?;
    wrote(&run, "a negative first and a separator", "-3,-1,1,3\n")?;
    let run = run_with(&seq, &["-w", "8", "10"], None)?;
    wrote(&run, "numbers of one width", "08\n09\n10\n")?;
    let run = run_with(&seq, &["1", "0.5", "2"], None)?;
    wrote(&run, "decimals", "1.0\n1.5\n2.0\n")
}

fn seq_refuses_a_bad_number() -> Result<(), Failure> {
    let run = run(&utility("seq"), &["1", "x"])?;
    refused(&run, "refusing x", "seq", 2)?;
    ensure(&run, "writing nothing", run.out.is_empty())
}

fn base64_encodes_and_decodes() -> Result<(), Failure> {
    let base64 = utility("base64");
    let run = run_with(&base64, &[], Some(b"hello, world\n"))?;
    wrote(&run, "encoding", "aGVsbG8sIHdvcmxkCg==\n")?;
    let encoded = file("b64-encoded", b"aGVsbG8s\nIHdvcmxkCg==\n")?;
    let run = run_with(&base64, &["-d", &encoded], None)?;
    wrote(&run, "decoding a file", "hello, world\n")?;
    let run = run_with(&base64, &["-w", "8"], Some(b"hello, world\n"))?;
    wrote(&run, "wrapping", "aGVsbG8s\nIHdvcmxk\nCg==\n")
}

fn base64_refuses_bad_input() -> Result<(), Failure> {
    let base64 = utility("base64");
    let run = run_with(&base64, &["-d"], Some(b"aGVs!bG8=\n"))?;
    refused(&run, "failing", "base64", 1)?;
    ensure(&run, "saying why", run.err.contains("invalid input"))?;
    let run = run_with(&base64, &["-d", "-i"], Some(b"aGVs!bG8=\n"))?;
    wrote(&run, "skipping it with -i", "hello")
}

/// The sum of `abc`, a line of a list for it, -c's verdicts, and JSON.
fn sums_hash_and_check(command: &str, abc: &str) -> Result<(), Failure> {
    let program = utility(command);
    let name = format!("{command}-abc");
    let data = file(&name, b"abc")?;
    let run = run(&program, &[&data])?;
    let line = format!("{abc}  {data}\n");
    wrote(&run, "the sum and the name", &line)?;
    let list = file(&format!("{command}-list"), line.as_bytes())?;
    let run = run_with(&program, &["-c", &list], None)?;
    wrote(&run, "checking a matching sum", &format!("{data}: OK\n"))?;
    let wrong = format!("{}  {data}\n", "0".repeat(abc.len()));
    let wrong = file(&format!("{command}-wrong"), wrong.as_bytes())?;
    let run = run_with(&program, &["-c", &wrong], None)?;
    ensure(&run, "failing a changed file", run.status == 1)?;
    ensure(
        &run,
        "naming it",
        run.out == format!("{data}: FAILED\n") && run.err.contains("did NOT match"),
    )?;
    let run = run_with(&program, &["--json"], Some(b"abc"))?;
    let json = format!("[{{\"path\":\"-\",\"hash\":\"{abc}\"}}]\n");
    wrote(&run, "JSON", &json)
}

/// A missing file is named, the others are still hashed, and the
/// command exits with 1.
fn sums_name_a_missing_file(command: &str) -> Result<(), Failure> {
    let program = utility(command);
    let data = file(&format!("{command}-present"), b"")?;
    let missing = path(&format!("{command}-missing"));
    let run = run(&program, &[&missing, &data])?;
    refused(&run, "failing", command, 1)?;
    ensure(
        &run,
        "naming the missing file",
        run.err.contains("-missing"),
    )?;
    ensure(
        &run,
        "still hashing the other",
        run.out.ends_with("-present\n"),
    )
}

fn sha256sum_hashes_and_checks() -> Result<(), Failure> {
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    sums_hash_and_check("sha256sum", abc)
}

fn sha256sum_names_a_missing_file() -> Result<(), Failure> {
    sums_name_a_missing_file("sha256sum")
}

fn md5sum_hashes_and_checks() -> Result<(), Failure> {
    sums_hash_and_check("md5sum", "900150983cd24fb0d6963f7d28e17f72")
}

fn md5sum_names_a_missing_file() -> Result<(), Failure> {
    sums_name_a_missing_file("md5sum")
}
