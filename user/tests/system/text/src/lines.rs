//! The checks of sort, uniq, cut and tr: each run from where the system
//! installs it, with an ordinary use and an error each.

use crate::{Check, Failure, ensure, file, path, refused, run, run_with, utility, wrote};

pub fn checks() -> Vec<(&'static str, Check)> {
    vec![
        ("sort_sorts", sort_sorts),
        ("sort_by_keys_and_numbers", sort_by_keys_and_numbers),
        (
            "sort_checks_and_writes_a_file",
            sort_checks_and_writes_a_file,
        ),
        ("sort_names_what_is_missing", sort_names_what_is_missing),
        ("sort_bad_key", sort_bad_key),
        ("uniq_merges", uniq_merges),
        ("uniq_names_what_is_missing", uniq_names_what_is_missing),
        ("uniq_bad_number", uniq_bad_number),
        ("cut_cuts", cut_cuts),
        ("cut_names_what_is_missing", cut_names_what_is_missing),
        ("cut_bad_list", cut_bad_list),
        ("tr_translates", tr_translates),
        ("tr_bad_operands", tr_bad_operands),
    ]
}

/// Lines are sorted by code point, from a file and standard input, and
/// in reverse with -r.
fn sort_sorts() -> Result<(), Failure> {
    let sort = utility("sort");
    let words = file("sort-words", "pear\nÄpfel\napple\nBanana\n".as_bytes())?;
    let got = run(&sort, &[&words])?;
    wrote(&got, "sorting a file", "Banana\napple\npear\nÄpfel\n")?;
    let got = run_with(&sort, &["-r", "-"], Some(b"a\nc\nb"))?;
    wrote(&got, "sorting standard input in reverse", "c\nb\na\n")?;
    let got = run_with(&sort, &["-u", "-f"], Some(b"b\nB\na\n"))?;
    wrote(&got, "one of the lines equal without case", "a\nb\n")
}

fn sort_by_keys_and_numbers() -> Result<(), Failure> {
    let sort = utility("sort");
    let got = run_with(&sort, &["-n"], Some(b"10\n9\n-1\n"))?;
    wrote(&got, "sorting numbers", "-1\n9\n10\n")?;
    let got = run_with(&sort, &["-h"], Some(b"1G\n2K\n512\n"))?;
    wrote(&got, "sorting sizes", "512\n2K\n1G\n")?;
    let got = run_with(&sort, &["-V"], Some(b"v1.10\nv1.9\n"))?;
    wrote(&got, "sorting versions", "v1.9\nv1.10\n")?;
    let input = b"b:2:x\na:10:y\nc:1:z\n";
    let got = run_with(&sort, &["-t", ":", "-k", "2,2n"], Some(input))?;
    wrote(&got, "sorting by a key", "c:1:z\nb:2:x\na:10:y\n")?;
    let got = run_with(&sort, &["-t:", "-k3r"], Some(input))?;
    wrote(
        &got,
        "sorting by a key in reverse",
        "c:1:z\na:10:y\nb:2:x\n",
    )
}

/// -c finds the first line out of order; -o writes to a file that is
/// also the input.
fn sort_checks_and_writes_a_file() -> Result<(), Failure> {
    let sort = utility("sort");
    let got = run_with(&sort, &["-c"], Some(b"a\nb\nb\n"))?;
    wrote(&got, "finding sorted lines sorted", "")?;
    let got = run_with(&sort, &["-c"], Some(b"a\nc\nb\n"))?;
    ensure(
        &got,
        "naming the line out of order",
        got.status == 1 && got.err.contains(":3: disorder: b"),
    )?;
    let list = file("sort-in-place", b"2\n1\n")?;
    let got = run(&sort, &["-o", &list, &list])?;
    wrote(&got, "sorting into a file", "")?;
    let sorted = std::fs::read(&list).unwrap_or_default();
    ensure(&got, "the file sorted in place", sorted == b"1\n2\n")
}

/// A missing file is named, the other files are still sorted, and sort
/// exits with 1.
fn sort_names_what_is_missing() -> Result<(), Failure> {
    let present = file("sort-present", b"b\na\n")?;
    let missing = path("sort-missing");
    let got = run(&utility("sort"), &[&missing, &present])?;
    refused(&got, "failing", "sort", 1)?;
    ensure(
        &got,
        "naming the missing file",
        got.err.contains("sort-missing"),
    )?;
    ensure(&got, "still sorting the other", got.out == "a\nb\n")
}

fn sort_bad_key() -> Result<(), Failure> {
    let got = run(&utility("sort"), &["-k", "0"])?;
    refused(&got, "refusing field 0", "sort", 2)?;
    let got = run(&utility("sort"), &["--nope"])?;
    refused(&got, "refusing an unknown option", "sort", 2)
}

fn uniq_merges() -> Result<(), Failure> {
    let uniq = utility("uniq");
    let input = b"a\na\nb\nA\nc\nc\n";
    let got = run_with(&uniq, &[], Some(input))?;
    wrote(&got, "merging adjacent lines", "a\nb\nA\nc\n")?;
    let got = run_with(&uniq, &["-c"], Some(input))?;
    wrote(
        &got,
        "counting",
        "      2 a\n      1 b\n      1 A\n      2 c\n",
    )?;
    let got = run_with(&uniq, &["-d"], Some(input))?;
    wrote(&got, "only repeated lines", "a\nc\n")?;
    let got = run_with(&uniq, &["-u", "-i"], Some(input))?;
    wrote(&got, "only unique lines without case", "b\n")?;
    let got = run_with(&uniq, &["-f", "1"], Some(b"1 x\n2 x\n3 y\n"))?;
    wrote(&got, "skipping a field", "1 x\n3 y\n")?;
    let list = file("uniq-in", b"x\nx\ny\n")?;
    let out = path("uniq-out");
    let got = run(&uniq, &[&list, &out])?;
    wrote(&got, "writing to the output file", "")?;
    let written = std::fs::read(&out).unwrap_or_default();
    ensure(&got, "the output file merged", written == b"x\ny\n")
}

fn uniq_names_what_is_missing() -> Result<(), Failure> {
    let got = run(&utility("uniq"), &[&path("uniq-missing")])?;
    refused(&got, "failing", "uniq", 1)?;
    ensure(
        &got,
        "naming the missing file",
        got.err.contains("uniq-missing"),
    )
}

fn uniq_bad_number() -> Result<(), Failure> {
    let got = run(&utility("uniq"), &["-s", "many"])?;
    refused(&got, "refusing a count that is no number", "uniq", 2)
}

fn cut_cuts() -> Result<(), Failure> {
    let cut = utility("cut");
    let got = run_with(&cut, &["-d", ":", "-f", "1,3"], Some(b"a:b:c:d\nplain\n"))?;
    wrote(&got, "cutting fields", "a:c\nplain\n")?;
    let got = run_with(&cut, &["-s", "-d:", "-f2-"], Some(b"a:b:c\nplain\n"))?;
    wrote(&got, "only lines with the delimiter", "b:c\n")?;
    let got = run_with(&cut, &["-c", "2-3"], Some("äöüx\n".as_bytes()))?;
    wrote(&got, "cutting characters", "öü\n")?;
    let got = run_with(&cut, &["-b", "1,3"], Some(b"abcd"))?;
    wrote(&got, "cutting bytes", "ac\n")?;
    let got = run_with(
        &cut,
        &["-f", "2", "--complement", "--output-delimiter", ","],
        Some(b"1\t2\t3\n"),
    )?;
    wrote(&got, "the other fields", "1,3\n")
}

fn cut_names_what_is_missing() -> Result<(), Failure> {
    let present = file("cut-present", b"x\ty\n")?;
    let missing = path("cut-missing");
    let got = run(&utility("cut"), &["-f", "2", &missing, &present])?;
    refused(&got, "failing", "cut", 1)?;
    ensure(
        &got,
        "naming the missing file",
        got.err.contains("cut-missing"),
    )?;
    ensure(&got, "still cutting the other", got.out == "y\n")
}

fn cut_bad_list() -> Result<(), Failure> {
    let got = run(&utility("cut"), &["-f", "3-1"])?;
    refused(&got, "refusing a backward range", "cut", 2)?;
    let got = run(&utility("cut"), &[])?;
    refused(&got, "refusing no list", "cut", 2)
}

fn tr_translates() -> Result<(), Failure> {
    let tr = utility("tr");
    let got = run_with(&tr, &["a-z", "A-Z"], Some("grüß dich\n".as_bytes()))?;
    wrote(&got, "translating a range", "GRüß DICH\n")?;
    let got = run_with(&tr, &["[:lower:]", "[:upper:]"], Some(b"abc"))?;
    wrote(&got, "translating classes", "ABC")?;
    let got = run_with(&tr, &["-d", "[:digit:]"], Some(b"a1b22c"))?;
    wrote(&got, "deleting", "abc")?;
    let got = run_with(&tr, &["-s", " "], Some(b"a   b  c"))?;
    wrote(&got, "squeezing", "a b c")?;
    let got = run_with(&tr, &["-cs", "a-z", "\\n"], Some(b"one, two  three"))?;
    wrote(&got, "the complement squeezed", "one\ntwo\nthree")?;
    let got = run_with(&tr, &["äö", "ao"], Some("Bär öl".as_bytes()))?;
    wrote(&got, "translating characters that are not ASCII", "Bar ol")
}

fn tr_bad_operands() -> Result<(), Failure> {
    let tr = utility("tr");
    let got = run(&tr, &["a"])?;
    refused(&got, "refusing a translation without set2", "tr", 2)?;
    let got = run(&tr, &["z-a", "x"])?;
    refused(&got, "refusing a backward range", "tr", 2)?;
    let got = run(&tr, &["[:nope:]", "x"])?;
    refused(&got, "refusing an unknown class", "tr", 2)
}
