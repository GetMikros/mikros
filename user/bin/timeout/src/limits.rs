//! What timeout takes: its durations, numbers with an optional fraction
//! and the suffixes `s`, `m`, `h` and `d`, its signals by name or
//! number, and the status it exits with (MIKSPEC-0098.R54). Its options
//! end at its first operand, the duration (MIKSPEC-0098.R31).

use mikros_cli::{FAILURE, Kind, Operand, Opt, Run, Spec};
use std::time::Duration;

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one(
            "duration",
            Kind::Text,
            "how long the command may run: a number with s, m, h or d, 0 for ever",
        ),
        Operand::some(
            "command",
            Kind::Command,
            "the command to run and its arguments",
        ),
    ],
    options: &[
        Opt::both(
            "signal",
            's',
            "the signal to send when the time is up, TERM without",
        )
        .takes("signal", Kind::Text),
        Opt::both(
            "kill-after",
            'k',
            "send KILL this long after the signal, if it still runs",
        )
        .takes("duration", Kind::Text),
        Opt::long(
            "preserve-status",
            "exit with the command's status also when the time was up",
        ),
        Opt::long(
            "foreground",
            "signal only the command, which may then read the terminal",
        ),
    ],
    stops: true,
    ..Spec::new(
        "timeout",
        "Runs the command, and sends it a signal when the duration is up. Exits\n\
         with 124 when the time was up, and with the command's status otherwise.",
    )
};

/// The status when the time was up.
pub const TIMED_OUT: u8 = 124;
pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
const NANOS: u128 = 1_000_000_000;

/// The signals by name, with their numbers on Mikros.
const SIGNALS: [(&str, i32); 31] = [
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("TRAP", 5),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("STKFLT", 16),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
    ("URG", 23),
    ("XCPU", 24),
    ("XFSZ", 25),
    ("VTALRM", 26),
    ("PROF", 27),
    ("WINCH", 28),
    ("IO", 29),
    ("PWR", 30),
    ("SYS", 31),
];

/// What a run asks for.
#[derive(Debug, PartialEq)]
pub struct Plan {
    /// How long the command may run, `None` for ever.
    pub limit: Option<Duration>,
    pub signal: i32,
    /// How long after the signal KILL follows, if at all.
    pub kill_after: Option<Duration>,
    pub preserve: bool,
    pub foreground: bool,
    /// The command and its arguments.
    pub command: Vec<String>,
}

/// What `run` asks for, or why its arguments are invalid.
pub fn plan(run: &Run) -> Result<Plan, String> {
    let (duration, command) = run.operands.split_first().ok_or("missing duration")?;
    let limit = duration_of(duration)?;
    let signal = match run.value("-s") {
        Some(name) => signal(name).ok_or_else(|| format!("invalid signal {name}"))?,
        None => SIGTERM,
    };
    let kill_after = match run.value("-k") {
        Some(text) => duration_of(text)?,
        None => None,
    };
    Ok(Plan {
        limit,
        signal,
        kill_after,
        preserve: run.has("--preserve-status"),
        foreground: run.has("--foreground"),
        command: command.to_vec(),
    })
}

/// The duration of `text`, `None` for 0, which sets no limit.
fn duration_of(text: &str) -> Result<Option<Duration>, String> {
    let nanos = nanos(text).ok_or_else(|| format!("invalid duration {text}"))?;
    let seconds = u64::try_from(nanos / NANOS).unwrap_or(u64::MAX);
    let duration = Duration::new(seconds, (nanos % NANOS) as u32);
    Ok(Some(duration).filter(|d| !d.is_zero()))
}

/// The nanoseconds of `text`: digits with an optional fraction after
/// `.`, at least one digit in all, and an optional suffix; saturating
/// where it is longer than nanoseconds hold.
pub fn nanos(text: &str) -> Option<u128> {
    let (number, unit) = match text.char_indices().last()? {
        (at, 's') => (&text[..at], 1),
        (at, 'm') => (&text[..at], 60),
        (at, 'h') => (&text[..at], 3600),
        (at, 'd') => (&text[..at], 86_400),
        _ => (text, 1),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.len() + fraction.len() == 0 || !digits(whole) || !digits(fraction) {
        return None;
    }
    let whole: u128 = match whole {
        "" => 0,
        w => w.parse().unwrap_or(u128::MAX),
    };
    let mut part: u128 = 0;
    let mut scale = NANOS;
    for b in fraction.bytes().take(9) {
        scale /= 10;
        part += u128::from(b - b'0') * scale;
    }
    Some(
        whole
            .saturating_mul(NANOS)
            .saturating_add(part)
            .saturating_mul(unit),
    )
}

/// The number of the signal `name`: a name, with or without `SIG`, in
/// any case, or a number up to 64.
pub fn signal(name: &str) -> Option<i32> {
    if let Ok(number) = name.parse::<i32>() {
        return Some(number).filter(|n| (0..=64).contains(n));
    }
    let upper = name.to_ascii_uppercase();
    let bare = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS.iter().find(|(n, _)| *n == bare).map(|(_, s)| *s)
}

/// The status timeout exits with: 124 when the time was up, unless the
/// command's is preserved, and otherwise the command's, for one that
/// exited with `code` or was ended by `signal`, 128 and the signal, as
/// shells give it.
pub fn status(timed_out: bool, preserve: bool, code: Option<i32>, signal: Option<i32>) -> u8 {
    if timed_out && !preserve {
        return TIMED_OUT;
    }
    match (code, signal) {
        (Some(code), _) => (code & 0xff) as u8,
        (None, Some(signal)) => (128 + (signal & 0x7f)) as u8,
        (None, None) => FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn parse(arguments: &[&str]) -> Parsed {
        COMMAND.parse(arguments.iter().map(|a| a.to_string()))
    }

    fn plan_of(arguments: &[&str]) -> Result<Plan, String> {
        match parse(arguments) {
            Parsed::Run(run) => plan(&run),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_options_until_the_duration() {
        let plan = plan_of(&[
            "-s",
            "INT",
            "-k1m",
            "--foreground",
            "1.5",
            "sleep",
            "-k",
            "9",
        ])
        .unwrap();
        assert_eq!(plan.limit, Some(Duration::from_millis(1500)));
        assert_eq!(plan.signal, 2);
        assert_eq!(plan.kill_after, Some(Duration::from_secs(60)));
        assert!(plan.foreground && !plan.preserve);
        assert_eq!(plan.command, ["sleep", "-k", "9"]);
        let plan = plan_of(&["0", "true"]).unwrap();
        assert_eq!(
            (plan.limit, plan.signal, plan.kill_after),
            (None, SIGTERM, None)
        );
        assert_eq!(
            parse(&["5"]),
            Parsed::Invalid("missing operand <command>...".into())
        );
    }

    #[test]
    fn refuses_bad_durations_and_signals() {
        assert_eq!(
            plan_of(&["soon", "true"]),
            Err("invalid duration soon".into())
        );
        assert_eq!(
            plan_of(&["-k", "x", "1", "true"]),
            Err("invalid duration x".into())
        );
        assert_eq!(
            plan_of(&["-s", "NOPE", "1", "true"]),
            Err("invalid signal NOPE".into())
        );
        assert_eq!(
            plan_of(&["-s", "65", "1", "true"]),
            Err("invalid signal 65".into())
        );
    }

    #[test]
    fn reads_durations() {
        assert_eq!(nanos("2"), Some(2 * NANOS));
        assert_eq!(nanos(".5m"), Some(30 * NANOS));
        assert_eq!(nanos("1h"), Some(3600 * NANOS));
        assert_eq!(nanos("2d"), Some(172_800 * NANOS));
        for bad in ["", "m", "-1", "1w", "1.2.3"] {
            assert_eq!(nanos(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_signals() {
        assert_eq!(signal("TERM"), Some(15));
        assert_eq!(signal("SIGKILL"), Some(9));
        assert_eq!(signal("hup"), Some(1));
        assert_eq!(signal("10"), Some(10));
        assert_eq!(signal("SIG"), None);
        assert_eq!(signal("-1"), None);
    }

    #[test]
    fn exits_with_124_or_the_command_status() {
        assert_eq!(status(true, false, None, Some(15)), 124);
        assert_eq!(status(true, true, None, Some(15)), 143);
        assert_eq!(status(false, false, Some(3), None), 3);
        assert_eq!(status(false, false, None, None), 1);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/timeout.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
