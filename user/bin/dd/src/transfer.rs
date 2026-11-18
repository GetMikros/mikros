//! What dd takes, as `key=value` operands, how it copies blocks from a
//! reader to a writer, and what it reports (MIKSPEC-0098.R38).

use mikros_cli::{Kind, Operand, Spec, size};
use std::io::{self, ErrorKind, Read, Write};
use std::time::Duration;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "operand",
        Kind::Text,
        "if=, of=, bs=, ibs=, obs=, count=, skip=, seek=, conv= and status=",
    )],
    ..Spec::new(
        "dd",
        "Copies blocks from if=<file>, or standard input, to of=<file>, or\n\
         standard output, and reports the records, bytes and time on standard\n\
         error. Sizes take K, M, G and T for powers of 1024 and KB, MB and GB\n\
         for powers of 1000.\n\
         \n\
         bs=<size>     read and write blocks of this size\n\
         ibs=<size>    read blocks of this size, 512 when absent\n\
         obs=<size>    write blocks of this size, 512 when absent\n\
         count=<n>     copy at most n input blocks\n\
         skip=<n>      leave out the first n input blocks\n\
         seek=<n>      start after the first n output blocks\n\
         conv=<list>   notrunc, noerror, sync and fsync, separated by commas\n\
         status=<what> none, noxfer or progress",
    )
};

/// The block size when none is given.
const BLOCK: usize = 512;

/// What standard error shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The records and the transfer at the end.
    Default,
    /// Only errors.
    None,
    /// The records but not the transfer.
    Noxfer,
    /// The transfer as it goes, too.
    Progress,
}

/// What the operands ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub input: Option<String>,
    pub output: Option<String>,
    pub ibs: usize,
    pub obs: usize,
    /// Whether `bs=` was given, so that each block read is written as
    /// it is.
    pub same_blocks: bool,
    pub count: Option<u64>,
    pub skip: u64,
    pub seek: u64,
    pub notrunc: bool,
    pub noerror: bool,
    pub sync: bool,
    pub fsync: bool,
    pub status: Status,
}

impl Settings {
    /// The settings of `operands`, or why they are invalid.
    pub fn parse(operands: &[String]) -> Result<Settings, String> {
        let mut s = Settings {
            input: None,
            output: None,
            ibs: BLOCK,
            obs: BLOCK,
            same_blocks: false,
            count: None,
            skip: 0,
            seek: 0,
            notrunc: false,
            noerror: false,
            sync: false,
            fsync: false,
            status: Status::Default,
        };
        let mut bs = None;
        for operand in operands {
            let Some((key, value)) = operand.split_once('=') else {
                return Err(format!("unknown operand {operand}"));
            };
            let number =
                || size::parse(value).ok_or_else(|| format!("invalid number in {operand}"));
            let block = || {
                size::parse(value)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("invalid size in {operand}"))
            };
            match key {
                "if" => s.input = Some(value.into()),
                "of" => s.output = Some(value.into()),
                "bs" => bs = Some(block()?),
                "ibs" => s.ibs = block()?,
                "obs" => s.obs = block()?,
                "count" => s.count = Some(number()?),
                "skip" => s.skip = number()?,
                "seek" => s.seek = number()?,
                "conv" => {
                    for conversion in value.split(',') {
                        match conversion {
                            "notrunc" => s.notrunc = true,
                            "noerror" => s.noerror = true,
                            "sync" => s.sync = true,
                            "fsync" => s.fsync = true,
                            other => return Err(format!("unknown conversion {other}")),
                        }
                    }
                }
                "status" => {
                    s.status = match value {
                        "none" => Status::None,
                        "noxfer" => Status::Noxfer,
                        "progress" => Status::Progress,
                        other => return Err(format!("invalid status {other}")),
                    }
                }
                _ => return Err(format!("unknown operand {operand}")),
            }
        }
        if let Some(bs) = bs {
            s.ibs = bs;
            s.obs = bs;
            s.same_blocks = true;
        }
        Ok(s)
    }

    /// The bytes that `skip=` leaves out of the input.
    pub fn skipped(&self) -> u64 {
        self.skip.saturating_mul(self.ibs as u64)
    }

    /// The bytes that `seek=` passes in the output.
    pub fn sought(&self) -> u64 {
        self.seek.saturating_mul(self.obs as u64)
    }
}

/// What a copy did: whole and partial records read and written, and the
/// bytes written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub full_in: u64,
    pub partial_in: u64,
    pub full_out: u64,
    pub partial_out: u64,
    pub bytes: u64,
}

/// The input of a copy.
pub trait Input: Read {
    /// Moves past `bytes` that could not be read; false when it cannot.
    fn pass(&mut self, bytes: u64) -> bool;
}

/// Why a copy stopped early.
#[derive(Debug)]
pub enum Failure {
    Read(io::Error),
    Write(io::Error),
}

/// Copies as `settings` say from `input` to `output`, counting in
/// `tally`. With `conv=noerror`, a block that fails to be read goes to
/// `error` and the copy goes on past it, with zeros in its place under
/// `conv=sync`. `progress` sees the tally after each block.
pub fn copy<I: Input, W: Write>(
    settings: &Settings,
    input: &mut I,
    output: &mut W,
    tally: &mut Tally,
    error: &mut dyn FnMut(&io::Error),
    progress: &mut dyn FnMut(&Tally),
) -> Result<(), Failure> {
    let ibs = settings.ibs;
    let mut block = vec![0u8; ibs];
    let mut pending: Vec<u8> = Vec::new();
    let mut blocks = 0u64;
    while settings.count.is_none_or(|most| blocks < most) {
        let read = loop {
            match input.read(&mut block) {
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                other => break other,
            }
        };
        blocks += 1;
        let mut length = match read {
            Ok(0) => break,
            Ok(n) => {
                match n == ibs {
                    true => tally.full_in += 1,
                    false => tally.partial_in += 1,
                }
                n
            }
            Err(e) if settings.noerror && input.pass(ibs as u64) => {
                error(&e);
                match settings.sync {
                    true => {
                        tally.partial_in += 1;
                        0
                    }
                    false => continue,
                }
            }
            Err(e) => {
                flush(settings, output, &mut pending, tally)?;
                return Err(Failure::Read(e));
            }
        };
        if settings.sync && length < ibs {
            block[length..].fill(0);
            length = ibs;
        }
        match settings.same_blocks {
            true => put(settings, output, &block[..length], tally)?,
            false => {
                pending.extend_from_slice(&block[..length]);
                let whole = pending.len() / settings.obs * settings.obs;
                for chunk in pending[..whole].chunks(settings.obs) {
                    put(settings, output, chunk, tally)?;
                }
                pending.drain(..whole);
            }
        }
        progress(tally);
    }
    flush(settings, output, &mut pending, tally)?;
    output.flush().map_err(Failure::Write)
}

/// Writes the bytes of a last, partial output block.
fn flush<W: Write>(
    settings: &Settings,
    output: &mut W,
    pending: &mut Vec<u8>,
    tally: &mut Tally,
) -> Result<(), Failure> {
    if !pending.is_empty() {
        put(settings, output, pending, tally)?;
        pending.clear();
    }
    Ok(())
}

/// Writes one output record.
fn put<W: Write>(
    settings: &Settings,
    output: &mut W,
    record: &[u8],
    tally: &mut Tally,
) -> Result<(), Failure> {
    output.write_all(record).map_err(Failure::Write)?;
    match record.len() == settings.obs {
        true => tally.full_out += 1,
        false => tally.partial_out += 1,
    }
    tally.bytes += record.len() as u64;
    Ok(())
}

/// Reads and drops `bytes` of `input`, for `skip=` on an input that
/// cannot seek; fewer when it ends first.
pub fn discard<R: Read>(input: &mut R, bytes: u64) -> io::Result<u64> {
    io::copy(&mut input.take(bytes), &mut io::sink())
}

/// Writes `bytes` zeros to `output`, for `seek=` on an output that
/// cannot seek.
pub fn zeros<W: Write>(output: &mut W, bytes: u64) -> io::Result<()> {
    io::copy(&mut io::repeat(0).take(bytes), output).map(drop)
}

/// The bytes copied, with a suffix from 1024 on, the time and the rate.
pub fn transfer(bytes: u64, elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    let rate = (bytes as f64 / seconds.max(1e-6)) as u64;
    let shown = match bytes < 1024 {
        true => String::new(),
        false => format!(" ({})", size::human(bytes)),
    };
    format!(
        "{bytes} bytes{shown} copied, {seconds:.3} s, {}/s",
        size::human(rate)
    )
}

/// What standard error shows at the end with `status`.
pub fn report(tally: &Tally, elapsed: Duration, status: Status) -> String {
    let records = format!(
        "{}+{} records in\n{}+{} records out\n",
        tally.full_in, tally.partial_in, tally.full_out, tally.partial_out
    );
    match status {
        Status::None => String::new(),
        Status::Noxfer => records,
        Status::Default | Status::Progress => {
            format!("{records}{}\n", transfer(tally.bytes, elapsed))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn settings(operands: &[&str]) -> Settings {
        let operands: Vec<String> = operands.iter().map(|o| o.to_string()).collect();
        Settings::parse(&operands).unwrap()
    }

    /// An input of `data` that reads at most `chunk` bytes at a time and
    /// fails once at `bad`.
    struct Fake {
        data: Cursor<Vec<u8>>,
        chunk: usize,
        bad: Option<u64>,
    }

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.bad == Some(self.data.position()) {
                return Err(io::Error::other("bad block"));
            }
            let most = buf.len().min(self.chunk);
            self.data.read(&mut buf[..most])
        }
    }

    impl Input for Fake {
        fn pass(&mut self, bytes: u64) -> bool {
            self.bad = None;
            self.data.set_position(self.data.position() + bytes);
            true
        }
    }

    fn fake(len: usize) -> Fake {
        Fake {
            data: Cursor::new((0..len).map(|i| (i % 251) as u8).collect()),
            chunk: usize::MAX,
            bad: None,
        }
    }

    fn run(s: &Settings, input: &mut Fake) -> (Result<(), Failure>, Vec<u8>, Tally, usize) {
        let mut out = Vec::new();
        let mut tally = Tally::default();
        let mut errors = 0;
        let done = copy(
            s,
            input,
            &mut out,
            &mut tally,
            &mut |_| errors += 1,
            &mut |_| {},
        );
        (done, out, tally, errors)
    }

    #[test]
    fn reads_operands() {
        let s = settings(&["if=a", "of=b", "bs=4K", "count=3", "skip=1", "seek=2KB"]);
        assert_eq!(s.input.as_deref(), Some("a"));
        assert_eq!((s.ibs, s.obs, s.same_blocks), (4096, 4096, true));
        assert_eq!((s.count, s.skip, s.seek), (Some(3), 1, 2000));
        assert_eq!((s.skipped(), s.sought()), (4096, 2000 * 4096));
        let s = settings(&["ibs=1M", "obs=100", "conv=notrunc,sync", "status=noxfer"]);
        assert_eq!((s.ibs, s.obs, s.same_blocks), (1 << 20, 100, false));
        assert!(s.notrunc && s.sync && !s.noerror && !s.fsync);
        assert_eq!(s.status, Status::Noxfer);
        let s = settings(&[]);
        assert_eq!(
            (s.ibs, s.obs, s.count, s.status),
            (512, 512, None, Status::Default)
        );
    }

    #[test]
    fn refuses_bad_operands() {
        let bad = |o: &str| Settings::parse(&[o.to_string()]).unwrap_err();
        assert_eq!(bad("nope"), "unknown operand nope");
        assert_eq!(bad("iff=x"), "unknown operand iff=x");
        assert_eq!(bad("bs=0"), "invalid size in bs=0");
        assert_eq!(bad("ibs=1Q"), "invalid size in ibs=1Q");
        assert_eq!(bad("count=x"), "invalid number in count=x");
        assert_eq!(bad("conv=ucase"), "unknown conversion ucase");
        assert_eq!(bad("status=loud"), "invalid status loud");
    }

    #[test]
    fn copies_whole_and_partial_blocks() {
        let s = settings(&["bs=100"]);
        let mut input = fake(250);
        let (done, out, tally, _) = run(&s, &mut input);
        assert!(done.is_ok());
        assert_eq!(out, input.data.get_ref()[..]);
        assert_eq!((tally.full_in, tally.partial_in), (2, 1));
        assert_eq!(
            (tally.full_out, tally.partial_out, tally.bytes),
            (2, 1, 250)
        );
        let s = settings(&["bs=100", "count=2"]);
        let (_, out, _, _) = run(&s, &mut fake(250));
        assert_eq!(out.len(), 200);
    }

    #[test]
    fn reblocks_output() {
        let s = settings(&["ibs=100", "obs=64"]);
        let mut input = fake(250);
        input.chunk = 30;
        let (_, out, tally, _) = run(&s, &mut input);
        assert_eq!(out, input.data.get_ref()[..]);
        assert_eq!((tally.full_in, tally.partial_in), (0, 9));
        assert_eq!((tally.full_out, tally.partial_out), (3, 1));
    }

    #[test]
    fn pads_with_sync() {
        let s = settings(&["bs=100", "conv=sync"]);
        let (_, out, tally, _) = run(&s, &mut fake(150));
        assert_eq!(out.len(), 200);
        assert!(out[150..].iter().all(|&b| b == 0));
        assert_eq!((tally.full_out, tally.partial_out), (2, 0));
    }

    #[test]
    fn stops_or_goes_on_after_a_read_error() {
        let s = settings(&["bs=100"]);
        let mut input = fake(300);
        input.bad = Some(100);
        let (done, out, tally, _) = run(&s, &mut input);
        assert!(matches!(done, Err(Failure::Read(_))));
        assert_eq!((out.len(), tally.full_in), (100, 1));
        let s = settings(&["bs=100", "conv=noerror,sync"]);
        let mut input = fake(300);
        input.bad = Some(100);
        let (done, out, tally, errors) = run(&s, &mut input);
        assert!(done.is_ok());
        assert_eq!(errors, 1);
        assert_eq!(out.len(), 300);
        assert!(out[100..200].iter().all(|&b| b == 0));
        assert_eq!(out[200..], input.data.get_ref()[200..]);
        assert_eq!((tally.full_in, tally.partial_in), (2, 1));
    }

    #[test]
    fn skips_and_seeks_without_seeking() {
        let mut input = Cursor::new(vec![1u8; 10]);
        assert_eq!(discard(&mut input, 4).unwrap(), 4);
        assert_eq!(discard(&mut input, 40).unwrap(), 6);
        let mut out = Vec::new();
        zeros(&mut out, 5).unwrap();
        assert_eq!(out, [0; 5]);
    }

    #[test]
    fn reports_records_bytes_and_time() {
        let tally = Tally {
            full_in: 2,
            partial_in: 1,
            full_out: 2,
            partial_out: 1,
            bytes: 2560,
        };
        let second = Duration::from_secs(1);
        assert_eq!(
            report(&tally, second, Status::Default),
            "2+1 records in\n2+1 records out\n2560 bytes (2.5K) copied, 1.000 s, 2.5K/s\n"
        );
        assert_eq!(
            report(&tally, second, Status::Noxfer),
            "2+1 records in\n2+1 records out\n"
        );
        assert_eq!(report(&tally, second, Status::None), "");
        assert_eq!(
            transfer(100, Duration::from_millis(500)),
            "100 bytes copied, 0.500 s, 200/s"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/dd.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
