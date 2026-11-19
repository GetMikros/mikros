//! What head takes, and how it copies the first lines or bytes of an
//! input, or all but the last (MIKSPEC-0098.R46, MIKSPEC-0098.R44).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, size};
use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to read, or - for standard input",
    )],
    options: &[
        Opt::both(
            "lines",
            'n',
            "write the first count lines, 10 when absent, or with -count all but the last",
        )
        .takes("count", Kind::Text),
        Opt::both(
            "bytes",
            'c',
            "write the first size bytes, or with -size all but the last",
        )
        .takes("size", Kind::Size),
        Opt::both("quiet", 'q', "write no names above the files"),
        Opt::both("verbose", 'v', "write the name above each file"),
    ],
    ..Spec::new(
        "head",
        "Writes the first 10 lines of each file, or of standard input, with\n\
         its name above it when there are several. Sizes take K, M, G and T\n\
         for powers of 1024, and KB, MB and GB for powers of 1000.",
    )
};

/// What head copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The first lines.
    Lines(u64),
    /// All but the last lines.
    AllButLines(u64),
    /// The first bytes.
    Bytes(u64),
    /// All but the last bytes.
    AllButBytes(u64),
}

/// A count of `-n` or `-c`: a number with a size's suffixes, all but the
/// last ones with a leading `-`.
fn count(text: &str) -> Option<(bool, u64)> {
    let (but, number) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    size::parse(number).map(|n| (but, n))
}

impl Part {
    /// The part that `run` asks for, by the later of `-n` and `-c`, or
    /// why its count is invalid.
    pub fn of(run: &Run) -> Result<Part, String> {
        let chosen = match run.last_of(&["-n", "-c"]) {
            Some("-c") => ('c', run.value("-c").unwrap_or_default()),
            Some(_) => ('n', run.value("-n").unwrap_or_default()),
            None => return Ok(Part::Lines(10)),
        };
        match (chosen.0, count(chosen.1)) {
            ('n', Some((false, n))) => Ok(Part::Lines(n)),
            ('n', Some((true, n))) => Ok(Part::AllButLines(n)),
            ('c', Some((false, n))) => Ok(Part::Bytes(n)),
            ('c', Some((true, n))) => Ok(Part::AllButBytes(n)),
            ('n', None) => Err(format!("invalid number of lines: {}", chosen.1)),
            _ => Err(format!("invalid number of bytes: {}", chosen.1)),
        }
    }
}

/// Whether to write the name above each file.
pub fn titled(run: &Run, files: usize) -> bool {
    match (run.has("-q"), run.has("-v")) {
        (_, true) => true,
        (true, false) => false,
        (false, false) => files > 1,
    }
}

/// The line above the file `name`, after a blank line unless it is the
/// first.
pub fn title(name: &str, first: bool, paint: Paint) -> String {
    let name = match name {
        "-" => "standard input",
        name => name,
    };
    let blank = if first { "" } else { "\n" };
    format!("{blank}==> {} <==\n", paint.paint(Role::Path, name))
}

/// Why a copy stopped: reading or writing failed.
#[derive(Debug)]
pub enum Failed {
    Read(io::Error),
    Write(io::Error),
}

/// Copies `part` of `input` to `out`.
pub fn copy(part: Part, input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
    match part {
        Part::Lines(n) => first_lines(n, input, out),
        Part::AllButLines(n) => all_but_lines(n, input, out),
        Part::Bytes(n) => {
            let mut taken = io::Read::take(input, n);
            copy_all(&mut taken, out)
        }
        Part::AllButBytes(n) => all_but_bytes(n, input, out),
    }
}

fn first_lines(n: u64, input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
    let mut line = Vec::new();
    for _ in 0..n {
        line.clear();
        if read_line(input, &mut line)? == 0 {
            break;
        }
        out.write_all(&line).map_err(Failed::Write)?;
    }
    Ok(())
}

fn all_but_lines(n: u64, input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
    let mut held: VecDeque<Vec<u8>> = VecDeque::new();
    loop {
        let mut line = Vec::new();
        if read_line(input, &mut line)? == 0 {
            return Ok(());
        }
        held.push_back(line);
        if held.len() as u64 > n
            && let Some(oldest) = held.pop_front()
        {
            out.write_all(&oldest).map_err(Failed::Write)?;
        }
    }
}

fn all_but_bytes(n: u64, input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    let mut held: Vec<u8> = Vec::new();
    loop {
        let available = match input.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Failed::Read(e)),
        };
        if available.is_empty() {
            return Ok(());
        }
        held.extend_from_slice(available);
        let used = available.len();
        input.consume(used);
        if held.len() > n {
            let free = held.len() - n;
            out.write_all(&held[..free]).map_err(Failed::Write)?;
            held.drain(..free);
        }
    }
}

fn copy_all(input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
    loop {
        let available = match input.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Failed::Read(e)),
        };
        if available.is_empty() {
            return Ok(());
        }
        out.write_all(available).map_err(Failed::Write)?;
        let used = available.len();
        input.consume(used);
    }
}

fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> Result<usize, Failed> {
    loop {
        match input.read_until(b'\n', line) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Failed::Read(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn run(arguments: &[&str]) -> Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    fn head(part: Part, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        copy(part, &mut &input[..], &mut out).unwrap();
        out
    }

    #[test]
    fn reads_counts_with_signs_and_suffixes() {
        assert_eq!(Part::of(&run(&[])), Ok(Part::Lines(10)));
        assert_eq!(Part::of(&run(&["-n", "3"])), Ok(Part::Lines(3)));
        assert_eq!(Part::of(&run(&["-n", "-3"])), Ok(Part::AllButLines(3)));
        assert_eq!(Part::of(&run(&["-n-3"])), Ok(Part::AllButLines(3)));
        assert_eq!(Part::of(&run(&["--lines=+4"])), Ok(Part::Lines(4)));
        assert_eq!(Part::of(&run(&["-c", "2K"])), Ok(Part::Bytes(2048)));
        assert_eq!(Part::of(&run(&["-c", "-1KB"])), Ok(Part::AllButBytes(1000)));
        assert_eq!(
            Part::of(&run(&["-n", "x"])),
            Err("invalid number of lines: x".into())
        );
        assert_eq!(
            Part::of(&run(&["-c", "--5"])),
            Err("invalid number of bytes: --5".into())
        );
        assert_eq!(Part::of(&run(&["-n1", "-c2"])), Ok(Part::Bytes(2)));
        assert_eq!(Part::of(&run(&["-c2", "-n1"])), Ok(Part::Lines(1)));
    }

    #[test]
    fn copies_the_first_lines_or_all_but_the_last() {
        let text = b"1\n2\n3\n4";
        assert_eq!(head(Part::Lines(2), text), b"1\n2\n");
        assert_eq!(head(Part::Lines(9), text), text);
        assert_eq!(head(Part::Lines(0), text), b"");
        assert_eq!(head(Part::AllButLines(1), text), b"1\n2\n3\n");
        assert_eq!(head(Part::AllButLines(3), text), b"1\n");
        assert_eq!(head(Part::AllButLines(9), text), b"");
        assert_eq!(head(Part::AllButLines(0), text), text);
    }

    #[test]
    fn copies_the_first_bytes_or_all_but_the_last() {
        let text = b"abcdef";
        assert_eq!(head(Part::Bytes(4), text), b"abcd");
        assert_eq!(head(Part::Bytes(40), text), text);
        assert_eq!(head(Part::AllButBytes(2), text), b"abcd");
        assert_eq!(head(Part::AllButBytes(6), text), b"");
        let long: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        let mut reader = io::BufReader::with_capacity(7, &long[..]);
        let mut out = Vec::new();
        copy(Part::AllButBytes(1000), &mut reader, &mut out).unwrap();
        assert_eq!(out, &long[..19_000]);
    }

    #[test]
    fn titles_files_when_there_are_several_or_asked() {
        assert!(!titled(&run(&[]), 1));
        assert!(titled(&run(&[]), 2));
        assert!(!titled(&run(&["-q"]), 2));
        assert!(titled(&run(&["-v"]), 1));
        assert_eq!(title("a", true, Paint::never()), "==> a <==\n");
        assert_eq!(
            title("-", false, Paint::never()),
            "\n==> standard input <==\n"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/head.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
