//! What date takes, and what a run asks for: which time, whether to set
//! it, and in which form to write it (MIKSPEC-0098.R53).

use crate::format::{Moment, strftime};
use mikros_cli::{Kind, Operand, Opt, Run, Spec, json_string, time};

const PRECISIONS: Kind = Kind::Choice(&["date", "seconds"]);

pub static COMMAND: Spec = Spec {
    operands: &[Operand::optional(
        "format",
        Kind::Text,
        "+ and the format to write the time in, with the conversions of strftime",
    )],
    options: &[
        Opt::both("utc", 'u', "write and read times in UTC"),
        Opt::both("date", 'd', "write this time instead of now").takes("date", Kind::Text),
        Opt::both("reference", 'r', "write the modified time of this file")
            .takes("file", Kind::File),
        Opt::both("set", 's', "set the system time to this time").takes("date", Kind::Text),
        Opt::both(
            "iso-8601",
            'I',
            "write ISO 8601: the date, the default, or with seconds",
        )
        .takes("what", PRECISIONS),
        Opt::both("rfc-email", 'R', "write RFC 5322, as e-mail dates"),
        Opt::long("rfc-3339", "write RFC 3339: the date, or with seconds")
            .takes("what", PRECISIONS),
        Opt::long("json", "write an object with utc, local and zone"),
    ],
    ..Spec::new(
        "date",
        "Writes the time now, or the one given, in local time with the zone, or\n\
         in the format given. A date is now, @ and seconds since 1970, or\n\
         YYYY-MM-DD, HH:MM[:SS] or both, with Z, UTC or +HH:MM after them.",
    )
};

/// Which time a run shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Now,
    /// The date of `-d`.
    Date(String),
    /// The modified time of the file of `-r`.
    Reference(String),
    /// The date of `-s`, to which the system time is set.
    Set(String),
}

/// How precise `-I` and `--rfc-3339` are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    Date,
    Seconds,
}

/// The form a run writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Default,
    /// A format of strftime, without its `+`.
    Format(String),
    Iso(Precision),
    Rfc3339(Precision),
    Email,
    Json,
}

/// What a run asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub source: Source,
    pub output: Output,
    pub utc: bool,
}

fn precision(value: Option<&str>) -> Precision {
    match value {
        Some("seconds") => Precision::Seconds,
        _ => Precision::Date,
    }
}

/// What `run` asks for, or why it is invalid: at most one of `-d`, `-r`
/// and `-s`, at most one form, and a format that starts with `+`.
pub fn request(run: &Run) -> Result<Request, String> {
    let mut sources = Vec::new();
    if let Some(date) = run.value("-d") {
        sources.push(Source::Date(date.into()));
    }
    if let Some(file) = run.value("-r") {
        sources.push(Source::Reference(file.into()));
    }
    if let Some(date) = run.value("-s") {
        sources.push(Source::Set(date.into()));
    }
    if sources.len() > 1 {
        return Err("only one of -d, -r and -s may be given".into());
    }
    let mut outputs = Vec::new();
    if let Some(format) = run.operands.first() {
        match format.strip_prefix('+') {
            Some(format) => outputs.push(Output::Format(format.into())),
            None => return Err(format!("the format must start with +: {format}")),
        }
    }
    if run.has("-I") {
        outputs.push(Output::Iso(precision(run.value("-I"))));
    }
    if run.has("--rfc-3339") {
        outputs.push(Output::Rfc3339(precision(run.value("--rfc-3339"))));
    }
    if run.has("-R") {
        outputs.push(Output::Email);
    }
    if run.has("--json") {
        outputs.push(Output::Json);
    }
    if outputs.len() > 1 {
        return Err("only one output format may be given".into());
    }
    Ok(Request {
        source: sources.pop().unwrap_or(Source::Now),
        output: outputs.pop().unwrap_or(Output::Default),
        utc: run.has("-u"),
    })
}

/// `arguments` with the value `date` added to `-I` and `--iso-8601`
/// where they have none, as their value may be left out.
pub fn with_iso_default(arguments: Vec<String>) -> Vec<String> {
    let takes_short = |c: char| {
        COMMAND
            .options
            .iter()
            .any(|o| o.short == Some(c) && o.value.is_some())
    };
    let takes_long = |name: &str| {
        COMMAND
            .options
            .iter()
            .any(|o| o.long == name && o.value.is_some())
    };
    let mut out = Vec::with_capacity(arguments.len());
    let mut value_next = false;
    let mut ended = false;
    for argument in arguments {
        if value_next || ended || argument == "-" || !argument.starts_with('-') {
            value_next = false;
            out.push(argument);
            continue;
        }
        if argument == "--" {
            ended = true;
        } else if argument == "--iso-8601" {
            out.push("--iso-8601=date".into());
            continue;
        } else if let Some(long) = argument.strip_prefix("--") {
            value_next = !long.contains('=') && takes_long(long);
        } else {
            let mut rewritten = None;
            for (at, c) in argument.char_indices().skip(1) {
                let last = at + c.len_utf8() == argument.len();
                if c == 'I' && last {
                    rewritten = Some(format!("{argument}date"));
                    break;
                }
                if takes_short(c) {
                    value_next = last;
                    break;
                }
            }
            if let Some(rewritten) = rewritten {
                out.push(rewritten);
                continue;
            }
        }
        out.push(argument);
    }
    out
}

/// The format of strftime that writes `output`, other than JSON.
fn format_of(output: &Output) -> &str {
    match output {
        Output::Default | Output::Json => "%Y-%m-%d %H:%M:%S %Z",
        Output::Format(format) => format,
        Output::Iso(Precision::Date) | Output::Rfc3339(Precision::Date) => "%Y-%m-%d",
        Output::Iso(Precision::Seconds) => "%Y-%m-%dT%H:%M:%S%:z",
        Output::Rfc3339(Precision::Seconds) => "%Y-%m-%d %H:%M:%S%:z",
        Output::Email => "%a, %d %b %Y %H:%M:%S %z",
    }
}

/// What date writes of `moment` in `output`, without colour.
pub fn written(moment: &Moment, output: &Output) -> String {
    match output {
        Output::Json => format!(
            "{{\"utc\":{},\"local\":{},\"zone\":{}}}\n",
            json_string(&time::utc(moment.seconds)),
            json_string(&strftime("%Y-%m-%dT%H:%M:%S%:z", moment)),
            json_string(&moment.zone)
        ),
        output => format!("{}\n", strftime(format_of(output), moment)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn words(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|a| a.to_string()).collect()
    }

    fn request_of(arguments: &[&str]) -> Result<Request, String> {
        match COMMAND.parse(with_iso_default(words(arguments))) {
            Parsed::Run(run) => request(&run),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gives_iso_8601_its_default() {
        let rewritten = |a: &[&str]| with_iso_default(words(a));
        assert_eq!(rewritten(&["-I"]), ["-Idate"]);
        assert_eq!(rewritten(&["-uI"]), ["-uIdate"]);
        assert_eq!(rewritten(&["--iso-8601"]), ["--iso-8601=date"]);
        assert_eq!(
            rewritten(&["-Iseconds", "--iso-8601=seconds"]),
            ["-Iseconds", "--iso-8601=seconds"]
        );
        // Values of other options, and operands, stay as they are.
        assert_eq!(rewritten(&["-d", "-I", "-dI"]), ["-d", "-I", "-dI"]);
        assert_eq!(
            rewritten(&["--date", "-I", "--date=x", "-I"]),
            ["--date", "-I", "--date=x", "-Idate"]
        );
        assert_eq!(rewritten(&["--", "-I"]), ["--", "-I"]);
    }

    #[test]
    fn reads_the_time_and_the_form() {
        let r = request_of(&["-u", "-d", "@0", "+%s"]).unwrap();
        assert_eq!(r.source, Source::Date("@0".into()));
        assert_eq!(r.output, Output::Format("%s".into()));
        assert!(r.utc);
        let r = request_of(&["-I"]).unwrap();
        assert_eq!(
            (r.source, r.output),
            (Source::Now, Output::Iso(Precision::Date))
        );
        let r = request_of(&["-Iseconds", "-r", "f"]).unwrap();
        assert_eq!(
            (r.source, r.output),
            (
                Source::Reference("f".into()),
                Output::Iso(Precision::Seconds)
            )
        );
        let r = request_of(&["--rfc-3339=seconds", "-s", "12:00"]).unwrap();
        assert_eq!(r.source, Source::Set("12:00".into()));
        assert_eq!(r.output, Output::Rfc3339(Precision::Seconds));
        assert_eq!(request_of(&["-R"]).unwrap().output, Output::Email);
        assert_eq!(request_of(&["--json"]).unwrap().output, Output::Json);
    }

    #[test]
    fn refuses_conflicts_and_formats_without_plus() {
        assert_eq!(
            request_of(&["-d", "now", "-r", "f"]),
            Err("only one of -d, -r and -s may be given".into())
        );
        assert_eq!(
            request_of(&["-R", "-I"]),
            Err("only one output format may be given".into())
        );
        assert_eq!(
            request_of(&["--json", "+%s"]),
            Err("only one output format may be given".into())
        );
        assert_eq!(
            request_of(&["%s"]),
            Err("the format must start with +: %s".into())
        );
        assert_eq!(
            COMMAND.parse(words(&["-Ihours"])),
            Parsed::Invalid("invalid value hours for --iso-8601".into())
        );
    }

    #[test]
    fn writes_each_form() {
        let m = Moment {
            seconds: 1_794_733_687,
            nanos: 0,
            offset: 3600,
            zone: "CET".into(),
        };
        let write = |output: Output| written(&m, &output);
        assert_eq!(write(Output::Default), "2026-11-15 10:08:07 CET\n");
        assert_eq!(write(Output::Format("%H".into())), "10\n");
        assert_eq!(write(Output::Iso(Precision::Date)), "2026-11-15\n");
        assert_eq!(
            write(Output::Iso(Precision::Seconds)),
            "2026-11-15T10:08:07+01:00\n"
        );
        assert_eq!(
            write(Output::Rfc3339(Precision::Seconds)),
            "2026-11-15 10:08:07+01:00\n"
        );
        assert_eq!(write(Output::Email), "Sun, 15 Nov 2026 10:08:07 +0100\n");
        assert_eq!(
            write(Output::Json),
            "{\"utc\":\"2026-11-15T09:08:07Z\",\"local\":\"2026-11-15T10:08:07+01:00\",\"zone\":\"CET\"}\n"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/date.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
