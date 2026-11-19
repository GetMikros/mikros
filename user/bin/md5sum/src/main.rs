//! md5sum, which prints and checks the MD5 sums of files; mikros-sums
//! writes and checks the lines, by this command's tables and algorithm.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

use md5::{Digest, Md5};
use mikros_cli::{Kind, Operand, Opt, Spec};
use mikros_sums::Algorithm;
use std::io::{self, Read};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to hash, or the lists to check, or - for standard input",
    )],
    options: &[
        Opt::both(
            "binary",
            'b',
            "mark the files as read in binary mode, with *",
        ),
        Opt::both(
            "text",
            't',
            "mark the files as read in text mode, the default",
        ),
        Opt::both(
            "zero",
            'z',
            "end each line with NUL, not line feed, and do not escape names",
        ),
        Opt::long("tag", "write lines in the BSD form: MD5 (name) = sum"),
        Opt::both("check", 'c', "read sums from the files and check them"),
        Opt::long(
            "quiet",
            "with -c, do not print OK for each file that matches",
        ),
        Opt::long("status", "with -c, print nothing: the exit status tells"),
        Opt::long(
            "strict",
            "with -c, fail on lines that are not lines of sums",
        ),
        Opt::both(
            "warn",
            'w',
            "with -c, warn about lines that are not lines of sums",
        ),
        Opt::long("json", "write the sums as a JSON array of objects"),
    ],
    ..Spec::new(
        "md5sum",
        "Prints the MD5 sum of each file, or of standard input, or with -c\n\
         checks the sums that lists of them give.\n\
         MD5 is broken: anyone can make a different file with the same sum.\n\
         Use it only to find accidental damage, and sha256sum where a file\n\
         may have been altered deliberately.",
    )
};

static ALGORITHM: Algorithm = Algorithm {
    name: "MD5",
    digits: 32,
    digest,
    command: &COMMAND,
};

/// The sum of what `input` holds, in lowercase hexadecimal.
fn digest(input: &mut dyn Read) -> io::Result<String> {
    let mut hasher = Md5::new();
    mikros_sums::feed(input, |bytes| hasher.update(bytes))?;
    Ok(mikros_sums::hex(&hasher.finalize()))
}

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    mikros_sums::run(&ALGORITHM, &mikros_command::list)
}

#[cfg(not(target_os = "mikros"))]
fn main() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        assert_eq!(
            digest(&mut &b"abc"[..]).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            digest(&mut &b""[..]).unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(ALGORITHM.digits, "900150983cd24fb0d6963f7d28e17f72".len());
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/md5sum.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
