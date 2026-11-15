//! What services is asked, and what it writes: a header line and one
//! line per service in aligned columns, a JSON array, or why a call on a
//! service failed.

use mikros_cli::{Arguments, Command, json_string};

pub const COMMAND: Command = Command {
    name: "services",
    operands: "[start|stop|restart <name>]",
    most: 2,
    about: "Lists the services with their state, process ID and restarts, or,\n\
            once an administrator allowed it, starts, stops or restarts one.",
    options: &[("--json", "write the services as a JSON array of objects")],
    values: &[],
};

/// A call on a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Start,
    Stop,
    Restart,
}

/// What services is asked to do.
#[derive(Debug, PartialEq)]
pub enum Request {
    List { json: bool },
    Call(Call, String),
    Help,
    Invalid(String),
}

pub fn read(arguments: Vec<String>) -> Request {
    let (options, mut operands) = match COMMAND.parse(arguments) {
        Arguments::Run {
            options, operands, ..
        } => (options, operands),
        Arguments::Help => return Request::Help,
        Arguments::Invalid(reason) => return Request::Invalid(reason),
    };
    let json = options.contains(&"--json");
    if operands.is_empty() {
        return Request::List { json };
    }
    let call = match operands[0].as_str() {
        "start" => Call::Start,
        "stop" => Call::Stop,
        "restart" => Call::Restart,
        other => return Request::Invalid(format!("unknown subcommand {other}")),
    };
    match operands.len() {
        2 => Request::Call(call, operands.swap_remove(1)),
        _ => Request::Invalid(format!("{} needs the name of a service", operands[0])),
    }
}

/// A service, as services shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    pub name: String,
    /// The state's name, in lowercase.
    pub state: String,
    pub on_demand: bool,
    /// 0 when it does not run.
    pub pid: u32,
    pub restarts: u32,
    pub exit_status: i32,
}

/// The header and lines of `services`: the name and state aligned to
/// the left, the process ID, or `-` when there is none, and the restarts
/// to the right.
pub fn text(services: &[Service]) -> String {
    let header = ["NAME", "STATE", "PID", "RESTARTS"];
    let mut rows = vec![header.map(String::from)];
    rows.extend(services.iter().map(|s| {
        let pid = match s.pid {
            0 => "-".into(),
            pid => pid.to_string(),
        };
        [s.name.clone(), s.state.clone(), pid, s.restarts.to_string()]
    }));
    let width = |column: usize| {
        rows.iter()
            .map(|r| r[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2)];
    rows.iter()
        .map(|r| {
            format!(
                "{:<w0$}  {:<w1$}  {:>w2$}  {:>w3$}\n",
                r[0],
                r[1],
                r[2],
                r[3],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = header[3].len(),
            )
        })
        .collect()
}

/// `services` as a JSON array of objects with the members `name`,
/// `state`, `on_demand`, `pid`, `restarts` and `exit_status`, on one line.
pub fn json(services: &[Service]) -> String {
    let objects: Vec<String> = services
        .iter()
        .map(|s| {
            format!(
                "{{\"name\":{},\"state\":{},\"on_demand\":{},\"pid\":{},\"restarts\":{},\
                 \"exit_status\":{}}}",
                json_string(&s.name),
                json_string(&s.state),
                s.on_demand,
                s.pid,
                s.restarts,
                s.exit_status
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

/// A name of a status or state as MIDL writes it in Rust, such as
/// `NotStarted`, in lowercase with underscores: `not_started`.
pub fn lowercase(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Why a call on the service `name` failed with the status named
/// `status`, as `lowercase` writes it, when the service is in `state`,
/// as far as the list tells.
pub fn failure(name: &str, status: &str, state: Option<&str>) -> String {
    let why = match (status, state) {
        ("not_found", _) => "no such service",
        ("not_supported", _) => "in the boot image",
        ("bad_state", Some(state)) => state,
        (status, _) => status,
    };
    format!("{name}: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_args(arguments: &[&str]) -> Request {
        read(arguments.iter().map(|a| a.to_string()).collect())
    }

    fn service(name: &str, state: &str, pid: u32, restarts: u32) -> Service {
        Service {
            name: name.into(),
            state: state.into(),
            on_demand: false,
            pid,
            restarts,
            exit_status: 0,
        }
    }

    #[test]
    fn reads_a_list_or_a_call() {
        assert_eq!(read_args(&[]), Request::List { json: false });
        assert_eq!(read_args(&["--json"]), Request::List { json: true });
        assert_eq!(
            read_args(&["stop", "termsvc"]),
            Request::Call(Call::Stop, "termsvc".into())
        );
        assert_eq!(
            read_args(&["restart", "--", "-odd"]),
            Request::Call(Call::Restart, "-odd".into())
        );
        assert_eq!(
            read_args(&["start", "a"]),
            Request::Call(Call::Start, "a".into())
        );
        assert_eq!(read_args(&["--help", "stop"]), Request::Help);
    }

    #[test]
    fn refuses_bad_subcommands_and_operands() {
        let invalid = |reason: &str| Request::Invalid(reason.into());
        assert_eq!(read_args(&["list"]), invalid("unknown subcommand list"));
        assert_eq!(
            read_args(&["stop"]),
            invalid("stop needs the name of a service")
        );
        assert_eq!(
            read_args(&["stop", "a", "b"]),
            invalid("unexpected argument b")
        );
    }

    #[test]
    fn writes_aligned_columns() {
        let services = [
            service("accountsvc", "running", 21, 0),
            service("termsvc", "not_started", 0, 12),
        ];
        assert_eq!(
            text(&services),
            "NAME        STATE        PID  RESTARTS\n\
             accountsvc  running       21         0\n\
             termsvc     not_started    -        12\n"
        );
        assert_eq!(text(&[]), "NAME  STATE  PID  RESTARTS\n");
    }

    #[test]
    fn writes_json() {
        assert_eq!(json(&[]), "[]\n");
        let on_demand = Service {
            on_demand: true,
            exit_status: -3,
            ..service("termsvc", "exited", 0, 1)
        };
        assert_eq!(
            json(&[on_demand]),
            "[{\"name\":\"termsvc\",\"state\":\"exited\",\"on_demand\":true,\"pid\":0,\
             \"restarts\":1,\"exit_status\":-3}]\n"
        );
    }

    #[test]
    fn names_failures() {
        assert_eq!(lowercase("NotStarted"), "not_started");
        assert_eq!(lowercase("Internal"), "internal");
        assert_eq!(failure("x", "not_found", None), "x: no such service");
        assert_eq!(
            failure("logsvc", "not_supported", Some("running")),
            "logsvc: in the boot image"
        );
        assert_eq!(failure("a", "bad_state", Some("running")), "a: running");
        assert_eq!(failure("a", "bad_state", None), "a: bad_state");
        assert_eq!(failure("a", "internal", Some("failed")), "a: internal");
    }
}
