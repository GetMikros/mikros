//! sha256sum, which prints and checks the SHA-256 sums of files; mikros-sums
//! writes and checks the lines, by this command's tables and algorithm.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

use mikros_cli::{Kind, Operand, Opt, Spec};
use mikros_sums::Algorithm;
use sha2::{Digest, Sha256};
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
        Opt::long("tag", "write lines in the BSD form: SHA256 (name) = sum"),
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
        "sha256sum",
        "Prints the SHA-256 sum of each file, or of standard input, or with -c\n\
         checks the sums that lists of them give.",
    )
};

static ALGORITHM: Algorithm = Algorithm {
    name: "SHA256",
    digits: 64,
    digest,
    command: &COMMAND,
};

/// The sum of what `input` holds, in lowercase hexadecimal.
fn digest(input: &mut dyn Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
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
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest(&mut &b""[..]).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            ALGORITHM.digits,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".len()
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/sha256sum.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
