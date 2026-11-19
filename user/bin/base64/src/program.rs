//! base64 on Mikros: it reads the file, or standard input, through the
//! standard library in pieces, and writes each piece's result as it goes
//! (MIKSPEC-0098.R44).

use crate::coding::{self, COMMAND, Decoder, Encoder};
use mikros_cli::{FAILURE, INVALID, Run, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

/// Why a run stopped early.
enum Stop {
    /// Reading the input failed.
    Read(io::Error),
    /// Writing the output failed.
    Write(io::Error),
    /// The input is not Base64.
    Invalid,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let Some(columns) = coding::columns(run.value("-w")) else {
        let value = run.value("-w").unwrap_or_default();
        complain(
            &COMMAND,
            &run,
            &format!("invalid number of columns: {value}"),
        );
        return ExitCode::from(INVALID);
    };
    let operand = run.operands.first().map_or("-", String::as_str);
    let input: io::Result<Box<dyn Read>> = match operand {
        "-" => Ok(Box::new(io::stdin().lock())),
        path => File::open(path).map(|f| Box::new(f) as Box<dyn Read>),
    };
    let mut input = match input {
        Ok(input) => input,
        Err(e) => {
            complain(&COMMAND, &run, &format!("{operand}: {e}"));
            return ExitCode::from(FAILURE);
        }
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
    let done = match run.has("-d") {
        true => decode(&mut input, &mut out, run.has("-i")),
        false => encode(&mut input, &mut out, columns),
    };
    let done = done.and_then(|()| out.flush().map_err(Stop::Write));
    match done {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(stop) => {
            report(&run, operand, stop);
            ExitCode::from(FAILURE)
        }
    }
}

fn report(run: &Run, operand: &str, stop: Stop) {
    match stop {
        Stop::Read(e) => complain(&COMMAND, run, &format!("{operand}: {e}")),
        Stop::Write(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
        Stop::Write(e) => complain(&COMMAND, run, &format!("standard output: {e}")),
        Stop::Invalid => complain(&COMMAND, run, "invalid input"),
    }
}

/// The next piece of `input`, empty at its end.
fn piece<'a>(input: &mut dyn Read, buffer: &'a mut [u8]) -> Result<&'a [u8], Stop> {
    loop {
        match input.read(buffer) {
            Ok(n) => return Ok(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Stop::Read(e)),
        }
    }
}

fn encode(input: &mut dyn Read, out: &mut dyn Write, columns: usize) -> Result<(), Stop> {
    let mut encoder = Encoder::new(columns);
    let mut buffer = vec![0; 48 * 1024];
    let mut text = Vec::new();
    loop {
        let bytes = piece(input, &mut buffer)?;
        if bytes.is_empty() {
            break;
        }
        encoder.feed(bytes, &mut text);
        out.write_all(&text).map_err(Stop::Write)?;
        text.clear();
    }
    encoder.finish(&mut text);
    out.write_all(&text).map_err(Stop::Write)
}

fn decode(input: &mut dyn Read, out: &mut dyn Write, ignore: bool) -> Result<(), Stop> {
    let mut decoder = Decoder::new(ignore);
    let mut buffer = vec![0; 64 * 1024];
    let mut bytes = Vec::new();
    loop {
        let text = piece(input, &mut buffer)?;
        if text.is_empty() {
            break;
        }
        let fed = decoder.feed(text, &mut bytes);
        out.write_all(&bytes).map_err(Stop::Write)?;
        bytes.clear();
        fed.map_err(|_| Stop::Invalid)?;
    }
    decoder.finish().map_err(|_| Stop::Invalid)
}
