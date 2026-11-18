//! What services is asked, and what it writes: a header line and one
//! line per service in aligned columns, a JSON array, or why a call on a
//! service failed.

use mikros_cli::table::{Cell, columns};
use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};

/// The operand of a call on a service.
const NAME: &[Operand] = &[Operand::one("name", Kind::Service, "the service")];
/// `--now`, of enabling and of disabling.
const START_NOW: &[Opt] = &[Opt::long("now", "also start the service")];
const STOP_NOW: &[Opt] = &[Opt::long("now", "also stop the service")];

pub static COMMAND: Spec = Spec {
    options: &[Opt::long(
        "json",
        "write the services as a JSON array of objects",
    )],
    subcommands: &[
        Spec {
            operands: NAME,
            ..Spec::new("start", "Starts a service.")
        },
        Spec {
            operands: NAME,
            ..Spec::new("stop", "Stops a service.")
        },
        Spec {
            operands: NAME,
            ..Spec::new("restart", "Restarts a service.")
        },
        Spec {
            operands: NAME,
            options: START_NOW,
            ..Spec::new("enable", "Enables a service, so that it starts at boot.")
        },
        Spec {
            operands: NAME,
            options: STOP_NOW,
            ..Spec::new(
                "disable",
                "Disables a service, so that it does not start at boot.",
            )
        },
    ],
    ..Spec::new(
        "services",
        "Lists the services with their state, process ID, restarts and whether\n\
         they are enabled, or, once an administrator allowed it, starts, stops,\n\
         restarts, enables or disables one.",
    )
};

/// A call on a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Start,
    Stop,
    Restart,
    /// Enabling or disabling it, and with `now` starting or stopping it
    /// too (MIKSPEC-0098.R30).
    Enable {
        now: bool,
    },
    Disable {
        now: bool,
    },
}

/// What services is asked to do.
#[derive(Debug, PartialEq)]
pub enum Request {
    List { json: bool },
    Call(Call, String),
}

/// What `run` asks.
pub fn read(run: &Run) -> Request {
    let now = run.has("--now");
    let call = match run.subcommand() {
        None => {
            return Request::List {
                json: run.has("--json"),
            };
        }
        Some("start") => Call::Start,
        Some("stop") => Call::Stop,
        Some("restart") => Call::Restart,
        Some("enable") => Call::Enable { now },
        Some(_) => Call::Disable { now },
    };
    Request::Call(call, run.operands[0].clone())
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
    pub enabled: bool,
}

/// The colour of a state: running ones as running, restarting ones as a
/// warning and failed ones as failed.
fn state_role(state: &str) -> Option<Role> {
    match state {
        "running" => Some(Role::Ok),
        "restarting" => Some(Role::Warning),
        "failed" => Some(Role::Error),
        _ => None,
    }
}

/// The header and lines of `services`: the name and state aligned to
/// the left, the process ID, or `-` when there is none, and the restarts
/// to the right, and `yes` or `no` for whether it is enabled.
pub fn text(services: &[Service], paint: Paint) -> String {
    let header = ["NAME", "STATE", "PID", "RESTARTS", "ENABLED"];
    let rows: Vec<Vec<Cell>> = services
        .iter()
        .map(|s| {
            let pid = match s.pid {
                0 => ("-".into(), Some(Role::Secondary)),
                pid => (pid.to_string(), Some(Role::Number)),
            };
            let enabled = if s.enabled { "yes" } else { "no" };
            vec![
                (s.name.clone(), None),
                (s.state.clone(), state_role(&s.state)),
                pid,
                (s.restarts.to_string(), Some(Role::Number)),
                (enabled.into(), None),
            ]
        })
        .collect();
    columns(&header, &rows, &[false, false, true, true, false], paint)
}

/// `services` as a JSON array of objects with the members `name`,
/// `state`, `on_demand`, `pid`, `restarts`, `exit_status` and `enabled`,
/// on one line.
pub fn json(services: &[Service]) -> String {
    let objects: Vec<String> = services
        .iter()
        .map(|s| {
            format!(
                "{{\"name\":{},\"state\":{},\"on_demand\":{},\"pid\":{},\"restarts\":{},\
                 \"exit_status\":{},\"enabled\":{}}}",
                json_string(&s.name),
                json_string(&s.state),
                s.on_demand,
                s.pid,
                s.restarts,
                s.exit_status,
                s.enabled
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
    use mikros_cli::Parsed;

    fn read_args(arguments: &[&str]) -> Result<Request, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Ok(read(&run)),
            Parsed::Invalid(reason) => Err(reason),
            Parsed::Help(path) => Err(format!("help for {path:?}")),
        }
    }

    fn service(name: &str, state: &str, pid: u32, restarts: u32) -> Service {
        Service {
            name: name.into(),
            state: state.into(),
            on_demand: false,
            pid,
            restarts,
            exit_status: 0,
            enabled: true,
        }
    }

    #[test]
    fn reads_a_list_or_a_call() {
        let call = |call, name: &str| Ok(Request::Call(call, name.into()));
        assert_eq!(read_args(&[]), Ok(Request::List { json: false }));
        assert_eq!(read_args(&["--json"]), Ok(Request::List { json: true }));
        assert_eq!(read_args(&["stop", "termsvc"]), call(Call::Stop, "termsvc"));
        assert_eq!(
            read_args(&["restart", "--", "-odd"]),
            call(Call::Restart, "-odd")
        );
        assert_eq!(read_args(&["start", "a"]), call(Call::Start, "a"));
        assert_eq!(
            read_args(&["enable", "--now", "a"]),
            call(Call::Enable { now: true }, "a")
        );
        assert_eq!(
            read_args(&["disable", "a"]),
            call(Call::Disable { now: false }, "a")
        );
        assert_eq!(
            read_args(&["--help", "stop"]),
            Err("help for [\"stop\"]".into())
        );
    }

    #[test]
    fn refuses_bad_subcommands_and_operands() {
        let invalid = |reason: &str| Err(reason.into());
        assert_eq!(read_args(&["list"]), invalid("unexpected argument list"));
        assert_eq!(read_args(&["stop"]), invalid("missing operand <name>"));
        assert_eq!(
            read_args(&["stop", "a", "b"]),
            invalid("unexpected argument b")
        );
        assert_eq!(
            read_args(&["stop", "--now", "a"]),
            invalid("unknown option --now")
        );
        assert_eq!(read_args(&["--now"]), invalid("unknown option --now"));
    }

    #[test]
    fn writes_aligned_columns() {
        let services = [
            service("accountsvc", "running", 21, 0),
            Service {
                enabled: false,
                ..service("termsvc", "not_started", 0, 12)
            },
        ];
        assert_eq!(
            text(&services, Paint::never()),
            "NAME        STATE        PID  RESTARTS  ENABLED\n\
             accountsvc  running       21         0  yes\n\
             termsvc     not_started    -        12  no\n"
        );
        assert_eq!(
            text(&[], Paint::never()),
            "NAME  STATE  PID  RESTARTS  ENABLED\n"
        );
    }

    #[test]
    fn colours_by_role_and_keeps_the_columns() {
        let on = Paint::new(Some("always"), false, None);
        let services = [service("a", "failed", 0, 3), service("bb", "running", 7, 0)];
        let paint = |sgr: &str, t: &str| format!("\x1b[{sgr}m{t}\x1b[0m");
        let grey = |t| paint("90", t);
        let number = |t| paint("38;2;21;193;160", t);
        let expected = [
            format!(
                "{}  {}    {}  {}  {}",
                grey("NAME"),
                grey("STATE"),
                grey("PID"),
                grey("RESTARTS"),
                grey("ENABLED")
            ),
            format!(
                "a     {}     {}         {}  yes",
                paint("38;2;231;76;60", "failed"),
                grey("-"),
                number("3")
            ),
            format!(
                "bb    {}    {}         {}  yes",
                paint("38;2;17;209;22", "running"),
                number("7"),
                number("0")
            ),
        ];
        assert_eq!(text(&services, on), expected.join("\n") + "\n");
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
             \"restarts\":1,\"exit_status\":-3,\"enabled\":true}]\n"
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

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/services.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
