//! What tail takes, and how it finds the last lines or bytes of an
//! input, or all from a line or byte on: from the end of a file it can
//! seek in, and by holding back the last ones of a stream
//! (MIKSPEC-0098.R46, MIKSPEC-0098.R44).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, size};
use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};

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
            "write the last count lines, 10 when absent, or with +count all from that line on",
        )
        .takes("count", Kind::Text),
        Opt::both(
            "bytes",
            'c',
            "write the last size bytes, or with +size all from that byte on",
        )
        .takes("size", Kind::Size),
        Opt::both("quiet", 'q', "write no names above the files"),
        Opt::both("verbose", 'v', "write the name above each file"),
        Opt::both("follow", 'f', "keep writing what is added to the files"),
        Opt::short(
            'F',
            "follow the files by name, also when they are replaced or appear later",
        ),
        Opt::long("pid", "with -f or -F, stop after process pid has ended").takes("pid", Kind::Int),
    ],
    ..Spec::new(
        "tail",
        "Writes the last 10 lines of each file, or of standard input, with its\n\
         name above it when there are several. Sizes take K, M, G and T for\n\
         powers of 1024, and KB, MB and GB for powers of 1000. Following a\n\
         file waits for changes where its file system can watch it, and\n\
         reads it again every second where it cannot.",
    )
};

/// What tail writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The last lines.
    LastLines(u64),
    /// All from a line on, counted from 1.
    FromLine(u64),
    /// The last bytes.
    LastBytes(u64),
    /// All from a byte on, counted from 1.
    FromByte(u64),
}

/// A count of `-n` or `-c`: a number with a size's suffixes, from the
/// start with a leading `+`, from the end with `-` or none.
fn count(text: &str) -> Option<(bool, u64)> {
    let (from, number) = match text.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('-').unwrap_or(text)),
    };
    size::parse(number).map(|n| (from, n))
}

impl Part {
    /// The part that `run` asks for, by the later of `-n` and `-c`, or
    /// why its count is invalid.
    pub fn of(run: &Run) -> Result<Part, String> {
        let (bytes, text) = match run.last_of(&["-n", "-c"]) {
            Some("-c") => (true, run.value("-c").unwrap_or_default()),
            Some(_) => (false, run.value("-n").unwrap_or_default()),
            None => return Ok(Part::LastLines(10)),
        };
        match (bytes, count(text)) {
            (false, Some((false, n))) => Ok(Part::LastLines(n)),
            (false, Some((true, n))) => Ok(Part::FromLine(n)),
            (true, Some((false, n))) => Ok(Part::LastBytes(n)),
            (true, Some((true, n))) => Ok(Part::FromByte(n)),
            (false, None) => Err(format!("invalid number of lines: {text}")),
            (true, None) => Err(format!("invalid number of bytes: {text}")),
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

/// Where in `file`, `size` bytes long, the part to write starts, for the
/// parts counted from the end, found by reading back from the end.
pub fn start(part: Part, file: &mut (impl Read + Seek), size: u64) -> io::Result<u64> {
    match part {
        Part::LastBytes(n) => Ok(size.saturating_sub(n)),
        Part::FromByte(n) => Ok(n.saturating_sub(1).min(size)),
        Part::FromLine(_) => Ok(0),
        Part::LastLines(n) => last_lines(file, size, n),
    }
}

/// Where the last `n` lines of `file` start. A last line without a line
/// feed counts as a line.
fn last_lines(file: &mut (impl Read + Seek), size: u64, n: u64) -> io::Result<u64> {
    if n == 0 {
        return Ok(size);
    }
    let mut block = vec![0; 64 * 1024];
    let mut end = size;
    let mut found = 0;
    while end > 0 {
        let begin = end.saturating_sub(block.len() as u64);
        let length = (end - begin) as usize;
        file.seek(SeekFrom::Start(begin))?;
        file.read_exact(&mut block[..length])?;
        for i in (0..length).rev() {
            let at = begin + i as u64;
            // The line feed that ends the file ends the last line.
            if block[i] == b'\n' && at + 1 != size {
                found += 1;
                if found == n {
                    return Ok(at + 1);
                }
            }
        }
        end = begin;
    }
    Ok(0)
}

/// Copies `part` of `input`, a stream, to `out`. `FromLine` and
/// `FromByte` start at once; the others hold back what may still be
/// among the last.
pub fn copy(part: Part, input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
    match part {
        Part::LastLines(n) => last_lines_of_stream(n, input, out),
        Part::FromLine(n) => {
            let mut line = Vec::new();
            for _ in 1..n {
                line.clear();
                if read_line(input, &mut line)? == 0 {
                    return Ok(());
                }
            }
            copy_all(input, out)
        }
        Part::LastBytes(n) => last_bytes_of_stream(n, input, out),
        Part::FromByte(n) => {
            let skip = n.saturating_sub(1);
            let mut skipped = io::Read::take(&mut *input, skip);
            io::copy(&mut skipped, &mut io::sink()).map_err(Failed::Read)?;
            copy_all(input, out)
        }
    }
}

fn last_lines_of_stream(
    n: u64,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<(), Failed> {
    let mut held: VecDeque<Vec<u8>> = VecDeque::new();
    loop {
        let mut line = Vec::new();
        if read_line(input, &mut line)? == 0 {
            break;
        }
        held.push_back(line);
        if held.len() as u64 > n {
            held.pop_front();
        }
    }
    for line in held {
        out.write_all(&line).map_err(Failed::Write)?;
    }
    Ok(())
}

fn last_bytes_of_stream(
    n: u64,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<(), Failed> {
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    let mut held: Vec<u8> = Vec::new();
    loop {
        let available = match input.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Failed::Read(e)),
        };
        if available.is_empty() {
            break;
        }
        held.extend_from_slice(available);
        let used = available.len();
        input.consume(used);
        // Drop what can no longer be among the last, now and then.
        if held.len() > n.saturating_mul(2).max(64 * 1024) {
            held.drain(..held.len() - n);
        }
    }
    let from = held.len().saturating_sub(n);
    out.write_all(&held[from..]).map_err(Failed::Write)
}

/// Copies all that is left of `input` to `out`.
pub fn copy_all(input: &mut impl BufRead, out: &mut impl Write) -> Result<(), Failed> {
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
    use std::io::Cursor;

    fn run(arguments: &[&str]) -> Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    fn streamed(part: Part, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        copy(part, &mut &input[..], &mut out).unwrap();
        out
    }

    /// What tail writes of `input` as a file it can seek in.
    fn sought(part: Part, input: &[u8]) -> Vec<u8> {
        let mut file = Cursor::new(input.to_vec());
        let from = start(part, &mut file, input.len() as u64).unwrap();
        file.set_position(from);
        let mut out = Vec::new();
        copy(
            match part {
                Part::FromLine(n) => Part::FromLine(n),
                _ => Part::FromByte(1),
            },
            &mut file,
            &mut out,
        )
        .unwrap();
        out
    }

    fn both(part: Part, input: &[u8]) -> Vec<u8> {
        let a = streamed(part, input);
        assert_eq!(a, sought(part, input), "{part:?} of {input:?}");
        a
    }

    #[test]
    fn reads_counts_with_signs_and_suffixes() {
        assert_eq!(Part::of(&run(&[])), Ok(Part::LastLines(10)));
        assert_eq!(Part::of(&run(&["-n", "3"])), Ok(Part::LastLines(3)));
        assert_eq!(Part::of(&run(&["-n", "-3"])), Ok(Part::LastLines(3)));
        assert_eq!(Part::of(&run(&["-n", "+3"])), Ok(Part::FromLine(3)));
        assert_eq!(Part::of(&run(&["-c+2K"])), Ok(Part::FromByte(2048)));
        assert_eq!(Part::of(&run(&["--bytes=5"])), Ok(Part::LastBytes(5)));
        assert_eq!(Part::of(&run(&["-n1", "-c2"])), Ok(Part::LastBytes(2)));
        assert_eq!(Part::of(&run(&["-c2", "-n1"])), Ok(Part::LastLines(1)));
        assert_eq!(
            Part::of(&run(&["-n", "x"])),
            Err("invalid number of lines: x".into())
        );
        assert_eq!(
            Part::of(&run(&["-c", "+-1"])),
            Err("invalid number of bytes: +-1".into())
        );
    }

    #[test]
    fn finds_the_last_lines_from_the_end_or_a_stream() {
        let text = b"1\n2\n3\n4\n";
        assert_eq!(both(Part::LastLines(2), text), b"3\n4\n");
        assert_eq!(both(Part::LastLines(9), text), text);
        assert_eq!(both(Part::LastLines(0), text), b"");
        assert_eq!(both(Part::LastLines(1), b"1\n2"), b"2");
        assert_eq!(both(Part::LastLines(2), b"\n\n\n"), b"\n\n");
        assert_eq!(both(Part::LastLines(3), b""), b"");
        let long: Vec<u8> = (0..200_000u32)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        assert_eq!(both(Part::LastLines(2), &long), b"199998\n199999\n");
    }

    #[test]
    fn writes_from_a_line_or_a_byte_on() {
        let text = b"1\n2\n3\n";
        assert_eq!(both(Part::FromLine(2), text), b"2\n3\n");
        assert_eq!(both(Part::FromLine(0), text), text);
        assert_eq!(both(Part::FromLine(1), text), text);
        assert_eq!(both(Part::FromLine(9), text), b"");
        assert_eq!(both(Part::FromByte(3), text), b"2\n3\n");
        assert_eq!(both(Part::FromByte(0), text), text);
        assert_eq!(both(Part::FromByte(99), text), b"");
    }

    #[test]
    fn writes_the_last_bytes() {
        assert_eq!(both(Part::LastBytes(2), b"abcdef"), b"ef");
        assert_eq!(both(Part::LastBytes(20), b"abc"), b"abc");
        let long: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let mut reader = io::BufReader::with_capacity(1000, &long[..]);
        let mut out = Vec::new();
        copy(Part::LastBytes(5000), &mut reader, &mut out).unwrap();
        assert_eq!(out, &long[295_000..]);
    }

    #[test]
    fn titles_files_when_there_are_several_or_asked() {
        assert!(titled(&run(&["-f"]), 2));
        assert!(!titled(&run(&["-q", "a", "b"]), 2));
        assert!(titled(&run(&["-v"]), 1));
        assert_eq!(title("b", false, Paint::never()), "\n==> b <==\n");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/tail.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
