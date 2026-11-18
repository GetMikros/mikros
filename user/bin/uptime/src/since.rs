//! What uptime takes, and how it writes the time and how long the system
//! has run (MIKSPEC-0098.R55), with no load averages.

use mikros_cli::{Opt, Paint, Role, Run, Spec, json_string, time};

pub static COMMAND: Spec = Spec {
    options: &[
        Opt::both("pretty", 'p', "only how long the system has run, in words"),
        Opt::both("since", 's', "only the time the system started"),
        Opt::long(
            "json",
            "write an object with the time, the seconds up and the start",
        ),
    ],
    ..Spec::new(
        "uptime",
        "Writes the time and how long the system has run, from the monotonic\n\
         clock.",
    )
};

/// A local time, as the C library breaks it down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// What a run writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// The time and how long the system has run.
    Line,
    Pretty,
    Since,
    Json,
}

/// What `run` asks for, or why it is invalid: at most one of `-p`, `-s`
/// and `--json`.
pub fn show(run: &Run) -> Result<Show, String> {
    let asked: Vec<Show> = [
        ("-p", Show::Pretty),
        ("-s", Show::Since),
        ("--json", Show::Json),
    ]
    .iter()
    .filter(|(option, _)| run.has(option))
    .map(|(_, show)| *show)
    .collect();
    match asked[..] {
        [] => Ok(Show::Line),
        [one] => Ok(one),
        _ => Err("-p, -s and --json cannot be used together".into()),
    }
}

/// `n` and `unit`, plural unless `n` is 1.
fn count(n: u64, unit: &str) -> String {
    match n {
        1 => format!("1 {unit}"),
        n => format!("{n} {unit}s"),
    }
}

/// The line: the time `now`, and `up` seconds as days, and hours and
/// minutes as `H:MM`, or minutes alone below an hour.
pub fn line(now: Civil, up: u64, paint: Paint) -> String {
    let days = up / 86_400;
    let (hours, minutes) = (up / 3600 % 24, up / 60 % 60);
    let mut run = String::new();
    if days > 0 {
        run.push_str(&count(days, "day"));
        run.push_str(", ");
    }
    match (days, hours) {
        (0, 0) => run.push_str(&format!("{minutes} min")),
        _ => run.push_str(&format!("{hours}:{minutes:02}")),
    }
    let clock = format!("{:02}:{:02}:{:02}", now.hour, now.minute, now.second);
    format!(
        "{} up {}\n",
        paint.paint(Role::Secondary, &clock),
        paint.paint(Role::Number, &run)
    )
}

/// `up` seconds in words, as `-p` writes them: days, hours and minutes,
/// leaving out those that are 0, and 0 minutes below a minute.
pub fn pretty(up: u64, paint: Paint) -> String {
    let parts: Vec<String> = [
        (up / 86_400, "day"),
        (up / 3600 % 24, "hour"),
        (up / 60 % 60, "minute"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, unit)| count(*n, unit))
    .collect();
    let words = match parts.is_empty() {
        true => "0 minutes".to_string(),
        false => parts.join(", "),
    };
    format!("up {}\n", paint.paint(Role::Number, &words))
}

/// The local time the system started, as `-s` writes it.
pub fn started(start: Civil, paint: Paint) -> String {
    let text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        start.year, start.month, start.day, start.hour, start.minute, start.second
    );
    format!("{}\n", paint.paint(Role::Secondary, &text))
}

/// The object of `--json`: `time`, now, `up`, the whole seconds the
/// system has run, and `since`, when it started, both times in RFC 3339
/// UTC, from `now`, seconds since 1970.
pub fn json(now: i64, up: u64) -> String {
    let since = now.saturating_sub(i64::try_from(up).unwrap_or(i64::MAX));
    format!(
        "{{\"time\":{},\"up\":{up},\"since\":{}}}\n",
        json_string(&time::utc(now)),
        json_string(&time::utc(since))
    )
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

    const NOW: Civil = Civil {
        year: 2026,
        month: 11,
        day: 18,
        hour: 9,
        minute: 5,
        second: 7,
    };

    #[test]
    fn takes_one_form() {
        assert_eq!(show(&run(&[])), Ok(Show::Line));
        assert_eq!(show(&run(&["-p"])), Ok(Show::Pretty));
        assert_eq!(show(&run(&["--since"])), Ok(Show::Since));
        assert_eq!(show(&run(&["--json"])), Ok(Show::Json));
        assert!(show(&run(&["-ps"])).is_err());
    }

    #[test]
    fn writes_the_line() {
        let never = Paint::never();
        assert_eq!(line(NOW, 59, never), "09:05:07 up 0 min\n");
        assert_eq!(line(NOW, 25 * 60, never), "09:05:07 up 25 min\n");
        assert_eq!(line(NOW, 3600 + 5 * 60, never), "09:05:07 up 1:05\n");
        assert_eq!(line(NOW, 86_400 + 120, never), "09:05:07 up 1 day, 0:02\n");
        assert_eq!(
            line(NOW, 3 * 86_400 + 4 * 3600, never),
            "09:05:07 up 3 days, 4:00\n"
        );
    }

    #[test]
    fn writes_words_the_start_and_json() {
        let never = Paint::never();
        assert_eq!(pretty(30, never), "up 0 minutes\n");
        assert_eq!(pretty(60, never), "up 1 minute\n");
        assert_eq!(
            pretty(2 * 86_400 + 3600 + 180, never),
            "up 2 days, 1 hour, 3 minutes\n"
        );
        assert_eq!(pretty(86_400, never), "up 1 day\n");
        assert_eq!(started(NOW, never), "2026-11-18 09:05:07\n");
        assert_eq!(
            json(1_794_733_687, 3600),
            "{\"time\":\"2026-11-15T09:08:07Z\",\"up\":3600,\"since\":\"2026-11-15T08:08:07Z\"}\n"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/uptime.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
