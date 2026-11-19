//! What logs is asked, which records it prints, and how: a line per
//! record, or a JSON array.

use mikros_cli::{Kind, Opt, Paint, Role, Run, Spec, json_string};

/// The names of the severities, from the lowest.
const SEVERITIES: [&str; 4] = ["debug", "info", "warning", "error"];

pub static COMMAND: Spec = Spec {
    options: &[
        Opt::long("app", "print only the records of app-id").takes("app-id", Kind::App),
        Opt::long("pid", "print only the records of process pid").takes("pid", Kind::Int),
        Opt::long("severity", "print only the records of severity or higher")
            .takes("severity", Kind::Choice(&SEVERITIES)),
        Opt::short('n', "print only the last count records that match").takes("count", Kind::Int),
        Opt::short('f', "print the new records as they come, until interrupted"),
        Opt::long("json", "write the records as a JSON array of objects"),
    ],
    ..Spec::new(
        "logs",
        "Prints the system log from its first record, once an administrator\n\
         allowed it, each record as <time> <severity> <app-id>[<pid>]: <message>.\n\
         The severities are debug, info, warning and error.",
    )
};

/// A record of the log.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub sequence: u64,
    /// The UTC time, in nanoseconds since 1970, or 0 when it was not
    /// known.
    pub utc: i64,
    pub pid: u32,
    pub app_id: String,
    /// The severity, 0 for debug to 3 for error.
    pub severity: u8,
    pub message: String,
}

/// The records to print.
#[derive(Debug, Default, PartialEq)]
pub struct Filter {
    pub app: Option<String>,
    pub pid: Option<u32>,
    /// The lowest severity.
    pub severity: u8,
}

impl Filter {
    pub fn matches(&self, entry: &Entry) -> bool {
        self.app.as_ref().is_none_or(|app| *app == entry.app_id)
            && self.pid.is_none_or(|pid| pid == entry.pid)
            && entry.severity >= self.severity
    }
}

/// What logs is asked to do.
#[derive(Debug, PartialEq)]
pub struct Request {
    pub filter: Filter,
    /// The most records to print, the last that match.
    pub last: Option<usize>,
    pub follow: bool,
    pub json: bool,
}

/// What `run` asks, or why its arguments are invalid.
pub fn read(run: &Run) -> Result<Request, String> {
    let follow = run.has("-f");
    let json = run.has("--json");
    if follow && json {
        return Err("-f and --json cannot go together".into());
    }
    let mut filter = Filter {
        app: run.value("--app").map(String::from),
        ..Filter::default()
    };
    if let Some(pid) = run.value("--pid") {
        match pid.parse() {
            Ok(pid) => filter.pid = Some(pid),
            Err(_) => return Err(format!("invalid process ID {pid}")),
        }
    }
    if let Some(name) = run.value("--severity") {
        match SEVERITIES.iter().position(|s| *s == name) {
            Some(severity) => filter.severity = severity as u8,
            None => return Err(format!("unknown severity {name}")),
        }
    }
    let last = match run.value("-n") {
        Some(count) => match count.parse() {
            Ok(count) => Some(count),
            Err(_) => return Err(format!("invalid count {count}")),
        },
        None => None,
    };
    Ok(Request {
        filter,
        last,
        follow,
        json,
    })
}

/// The name of `severity`, or its number when it has none.
fn severity(severity: u8) -> String {
    match SEVERITIES.get(severity as usize) {
        Some(name) => (*name).into(),
        None => severity.to_string(),
    }
}

/// The UTC time `utc` in RFC 3339, with milliseconds, or none when it is
/// 0.
fn time(utc: i64) -> Option<String> {
    if utc == 0 {
        return None;
    }
    let seconds = utc.div_euclid(1_000_000_000);
    let millis = utc.rem_euclid(1_000_000_000) / 1_000_000;
    let (year, month, day) = mikros_tz::civil(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    ))
}

/// `message` as one line: its backslashes as `\\`, its line feeds as
/// `\n`, and every other control character as `?`, so that it holds no
/// escape sequence.
fn one_line(message: &str) -> String {
    let mut line = String::with_capacity(message.len());
    for c in message.chars() {
        match c {
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            c if c.is_control() => line.push('?'),
            c => line.push(c),
        }
    }
    line
}

/// The line of `entry`, without its line feed: the time in the
/// secondary colour, the severities error and warning in theirs, the
/// process ID as a number and the message as free text, or all of it in
/// the secondary colour for a debug record.
pub fn line(entry: &Entry, paint: Paint) -> String {
    let time = time(entry.utc);
    let time = time.as_deref().unwrap_or("-");
    let severity = severity(entry.severity);
    let message = one_line(&entry.message);
    if entry.severity == 0 {
        let line = format!(
            "{time} {severity} {}[{}]: {message}",
            entry.app_id, entry.pid
        );
        return paint.paint(Role::Secondary, &line);
    }
    let severity = match entry.severity {
        3 => paint.paint(Role::Error, &severity),
        2 => paint.paint(Role::Warning, &severity),
        _ => severity,
    };
    format!(
        "{} {severity} {}[{}]: {}",
        paint.paint(Role::Secondary, time),
        entry.app_id,
        paint.paint(Role::Number, &entry.pid.to_string()),
        paint.paint(Role::String, &message)
    )
}

/// `entries` as a JSON array of objects with the members `sequence`,
/// `time`, `pid`, `app_id`, `severity` and `message`, on one line.
pub fn json(entries: &[Entry]) -> String {
    let objects: Vec<String> = entries
        .iter()
        .map(|e| {
            let time = match time(e.utc) {
                Some(time) => json_string(&time),
                None => "null".into(),
            };
            format!(
                "{{\"sequence\":{},\"time\":{time},\"pid\":{},\"app_id\":{},\
                 \"severity\":{},\"message\":{}}}",
                e.sequence,
                e.pid,
                json_string(&e.app_id),
                json_string(&severity(e.severity)),
                json_string(&e.message)
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn read_args(arguments: &[&str]) -> Result<Request, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => read(&run),
            Parsed::Invalid(reason) => Err(reason),
            Parsed::Help(path) => Err(format!("help for {path:?}")),
        }
    }

    fn entry(utc: i64, severity: u8, message: &str) -> Entry {
        Entry {
            sequence: 7,
            utc,
            pid: 12,
            app_id: "system.timesvc".into(),
            severity,
            message: message.into(),
        }
    }

    // 2026-11-15T09:08:07.123456789Z.
    const UTC: i64 = 1_794_733_687_123_456_789;

    #[test]
    fn reads_filters_counts_and_follow() {
        assert_eq!(
            read_args(&[]),
            Ok(Request {
                filter: Filter::default(),
                last: None,
                follow: false,
                json: false,
            })
        );
        let args = [
            "--app",
            "system.a",
            "--pid=9",
            "--severity",
            "warning",
            "-n3",
            "-f",
        ];
        assert_eq!(
            read_args(&args),
            Ok(Request {
                filter: Filter {
                    app: Some("system.a".into()),
                    pid: Some(9),
                    severity: 2,
                },
                last: Some(3),
                follow: true,
                json: false,
            })
        );
        assert_eq!(read_args(&["-fn", "2"]).map(|r| r.last), Ok(Some(2)));
        assert_eq!(read_args(&["--help"]), Err("help for []".into()));
    }

    #[test]
    fn refuses_bad_values_and_follow_with_json() {
        let invalid = |reason: &str| Err(reason.into());
        assert_eq!(
            read_args(&["-f", "--json"]),
            invalid("-f and --json cannot go together")
        );
        assert_eq!(read_args(&["--pid", "x"]), invalid("invalid process ID x"));
        assert_eq!(read_args(&["-n", "-1"]), invalid("invalid count -1"));
        assert_eq!(
            read_args(&["--severity", "fatal"]),
            invalid("invalid value fatal for --severity")
        );
        assert_eq!(read_args(&["x"]), invalid("unexpected argument x"));
        assert_eq!(read_args(&["-n"]), invalid("option -n needs a value"));
    }

    #[test]
    fn matches_app_pid_and_severity_or_higher() {
        let filter = Filter {
            app: Some("system.timesvc".into()),
            pid: Some(12),
            severity: 1,
        };
        assert!(filter.matches(&entry(0, 1, "")));
        assert!(filter.matches(&entry(0, 3, "")));
        assert!(!filter.matches(&entry(0, 0, "")));
        let other = Entry {
            pid: 13,
            ..entry(0, 3, "")
        };
        assert!(!filter.matches(&other));
        assert!(Filter::default().matches(&other));
    }

    #[test]
    fn prints_a_line_per_record() {
        let never = Paint::never();
        assert_eq!(
            line(&entry(UTC, 2, "clock set"), never),
            "2026-11-15T09:08:07.123Z warning system.timesvc[12]: clock set"
        );
        assert_eq!(
            line(&entry(0, 0, "a\\b\nc\x1b[2Jd\u{9b}"), never),
            "- debug system.timesvc[12]: a\\\\b\\nc?[2Jd?"
        );
        assert_eq!(time(-1), Some("1969-12-31T23:59:59.999Z".into()));
        assert_eq!(severity(9), "9");
    }

    #[test]
    fn colours_by_role() {
        let on = Paint::new(Some("always"), false, None);
        let paint = |sgr: &str, t: &str| format!("\x1b[{sgr}m{t}\x1b[0m");
        assert_eq!(
            line(&entry(0, 3, "disk \x1b gone"), on),
            format!(
                "{} {} system.timesvc[12]: disk ? gone",
                paint("90", "-"),
                paint("31", "error"),
            )
        );
        assert_eq!(
            line(&entry(0, 2, "w"), on).split(' ').nth(1),
            Some(paint("33", "warning").as_str())
        );
        assert_eq!(line(&entry(0, 1, "i"), on).split(' ').nth(1), Some("info"));
        assert_eq!(
            line(&entry(0, 0, "d"), on),
            paint("90", "- debug system.timesvc[12]: d")
        );
    }

    #[test]
    fn writes_json() {
        assert_eq!(json(&[]), "[]\n");
        assert_eq!(
            json(&[entry(UTC, 3, "a\"b"), entry(0, 1, "c")]),
            "[{\"sequence\":7,\"time\":\"2026-11-15T09:08:07.123Z\",\"pid\":12,\
             \"app_id\":\"system.timesvc\",\"severity\":\"error\",\"message\":\"a\\\"b\"},\
             {\"sequence\":7,\"time\":null,\"pid\":12,\"app_id\":\"system.timesvc\",\
             \"severity\":\"info\",\"message\":\"c\"}]\n"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/logs.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
