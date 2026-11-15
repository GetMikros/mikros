//! What logs is asked, which records it prints, and how: a line per
//! record, or a JSON array.

use mikros_cli::{Arguments, Command, json_string, value};

pub const COMMAND: Command = Command {
    name: "logs",
    operands: "",
    most: 0,
    about: "Prints the system log from its first record, once an administrator\n\
            allowed it, each record as <time> <severity> <app-id>[<pid>]: <message>.\n\
            The severities are debug, info, warning and error.",
    options: &[
        (
            "-f",
            "print the new records as they come, until interrupted",
        ),
        ("--json", "write the records as a JSON array of objects"),
    ],
    values: &[
        ("--app", "app-id", "print only the records of app-id"),
        ("--pid", "pid", "print only the records of process pid"),
        (
            "--severity",
            "severity",
            "print only the records of severity or higher",
        ),
        (
            "-n",
            "count",
            "print only the last count records that match",
        ),
    ],
};

/// The names of the severities, from the lowest.
const SEVERITIES: [&str; 4] = ["debug", "info", "warning", "error"];

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
pub enum Request {
    Print {
        filter: Filter,
        /// The most records to print, the last that match.
        last: Option<usize>,
        follow: bool,
        json: bool,
    },
    Help,
    Invalid(String),
}

pub fn read(arguments: Vec<String>) -> Request {
    let (options, values) = match COMMAND.parse(arguments) {
        Arguments::Run {
            options, values, ..
        } => (options, values),
        Arguments::Help => return Request::Help,
        Arguments::Invalid(reason) => return Request::Invalid(reason),
    };
    let follow = options.contains(&"-f");
    let json = options.contains(&"--json");
    if follow && json {
        return Request::Invalid("-f and --json cannot go together".into());
    }
    let mut filter = Filter {
        app: value(&values, "--app").map(String::from),
        ..Filter::default()
    };
    if let Some(pid) = value(&values, "--pid") {
        match pid.parse() {
            Ok(pid) => filter.pid = Some(pid),
            Err(_) => return Request::Invalid(format!("invalid process ID {pid}")),
        }
    }
    if let Some(name) = value(&values, "--severity") {
        match SEVERITIES.iter().position(|s| *s == name) {
            Some(severity) => filter.severity = severity as u8,
            None => return Request::Invalid(format!("unknown severity {name}")),
        }
    }
    let last = match value(&values, "-n") {
        Some(count) => match count.parse() {
            Ok(count) => Some(count),
            Err(_) => return Request::Invalid(format!("invalid count {count}")),
        },
        None => None,
    };
    Request::Print {
        filter,
        last,
        follow,
        json,
    }
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

/// The line of `entry`, without its line feed.
pub fn line(entry: &Entry) -> String {
    format!(
        "{} {} {}[{}]: {}",
        time(entry.utc).as_deref().unwrap_or("-"),
        severity(entry.severity),
        entry.app_id,
        entry.pid,
        one_line(&entry.message)
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

    fn read_args(arguments: &[&str]) -> Request {
        read(arguments.iter().map(|a| a.to_string()).collect())
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
            Request::Print {
                filter: Filter::default(),
                last: None,
                follow: false,
                json: false,
            }
        );
        let args = [
            "--app",
            "system.a",
            "--pid",
            "9",
            "--severity",
            "warning",
            "-n",
            "3",
            "-f",
        ];
        assert_eq!(
            read_args(&args),
            Request::Print {
                filter: Filter {
                    app: Some("system.a".into()),
                    pid: Some(9),
                    severity: 2,
                },
                last: Some(3),
                follow: true,
                json: false,
            }
        );
        assert_eq!(read_args(&["--help"]), Request::Help);
    }

    #[test]
    fn refuses_bad_values_and_follow_with_json() {
        let invalid = |reason: &str| Request::Invalid(reason.into());
        assert_eq!(
            read_args(&["-f", "--json"]),
            invalid("-f and --json cannot go together")
        );
        assert_eq!(read_args(&["--pid", "x"]), invalid("invalid process ID x"));
        assert_eq!(read_args(&["-n", "-1"]), invalid("invalid count -1"));
        assert_eq!(
            read_args(&["--severity", "fatal"]),
            invalid("unknown severity fatal")
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
        assert_eq!(
            line(&entry(UTC, 2, "clock set")),
            "2026-11-15T09:08:07.123Z warning system.timesvc[12]: clock set"
        );
        assert_eq!(
            line(&entry(0, 0, "a\\b\nc\x1b[2Jd\u{9b}")),
            "- debug system.timesvc[12]: a\\\\b\\nc?[2Jd?"
        );
        assert_eq!(time(-1), Some("1969-12-31T23:59:59.999Z".into()));
        assert_eq!(severity(9), "9");
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
}
