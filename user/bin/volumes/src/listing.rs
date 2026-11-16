//! What volumes is asked, and what it writes: a header line and one line
//! per volume in aligned columns, a JSON array, or why a call on a volume
//! failed.

use mikros_cli::{Arguments, Command, json_string};

pub const COMMAND: Command = Command {
    name: "volumes",
    operands: "[eject <volume>|mount <id>]",
    most: 2,
    about: "Lists the volumes with their names, IDs, file systems and states,\n\
            or ejects one by its name or ID, or mounts one by its ID.",
    options: &[("--json", "write the volumes as a JSON array of objects")],
    values: &[],
};

/// A call on a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Eject,
    Mount,
}

/// What volumes is asked to do.
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
        "eject" => Call::Eject,
        "mount" => Call::Mount,
        other => return Request::Invalid(format!("unknown subcommand {other}")),
    };
    if json {
        return Request::Invalid(format!("--json does not go with {}", operands[0]));
    }
    match operands.len() {
        2 => Request::Call(call, operands.swap_remove(1)),
        _ => Request::Invalid(format!("{} needs a volume", operands[0])),
    }
}

/// The state of a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Mounted,
    Hidden,
    Ejected,
}

impl State {
    /// The state of a volume that is `mounted`, or else `hidden` or not.
    pub fn of(mounted: bool, hidden: bool) -> State {
        match (mounted, hidden) {
            (true, _) => State::Mounted,
            (false, true) => State::Hidden,
            (false, false) => State::Ejected,
        }
    }

    fn name(self) -> &'static str {
        match self {
            State::Mounted => "mounted",
            State::Hidden => "hidden",
            State::Ejected => "ejected",
        }
    }
}

/// A volume, as volumes shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    /// Its name in `/volumes`, when it is mounted.
    pub name: Option<String>,
    pub id: String,
    /// Its file system's type, when it is mounted.
    pub kind: Option<String>,
    pub state: State,
    pub removable: bool,
}

/// The header and lines of `volumes`, each column aligned to the left,
/// with `-` for a name or type that a volume has not.
pub fn text(volumes: &[Volume]) -> String {
    let header = ["NAME", "ID", "TYPE", "STATE", "REMOVABLE"];
    let mut rows = vec![header.map(String::from)];
    rows.extend(volumes.iter().map(|v| {
        [
            v.name.clone().unwrap_or_else(|| "-".into()),
            v.id.clone(),
            v.kind.clone().unwrap_or_else(|| "-".into()),
            v.state.name().into(),
            match v.removable {
                true => "yes".into(),
                false => "no".into(),
            },
        ]
    }));
    let width = |column: usize| {
        rows.iter()
            .map(|r| r[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];
    rows.iter()
        .map(|r| {
            format!(
                "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}\n",
                r[0],
                r[1],
                r[2],
                r[3],
                r[4],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = widths[3],
            )
        })
        .collect()
}

/// `volumes` as a JSON array of objects with the members `name`, `id`,
/// `type`, `state` and `removable`, on one line.
pub fn json(volumes: &[Volume]) -> String {
    let optional = |text: &Option<String>| match text {
        Some(text) => json_string(text),
        None => "null".into(),
    };
    let objects: Vec<String> = volumes
        .iter()
        .map(|v| {
            format!(
                "{{\"name\":{},\"id\":{},\"type\":{},\"state\":{},\"removable\":{}}}",
                optional(&v.name),
                json_string(&v.id),
                optional(&v.kind),
                json_string(v.state.name()),
                v.removable
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

/// A name of a status as MIDL writes it in Rust, such as `BadState`, in
/// lowercase with underscores: `bad_state`.
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

/// Why `call` on `volume` failed with the status named `status`, as
/// `lowercase` writes it.
pub fn failure(call: Call, volume: &str, status: &str) -> String {
    match (status, call) {
        ("not_found", _) => format!("{volume}: no such volume"),
        ("bad_state", Call::Eject) => format!("{volume}: already ejected"),
        ("bad_state", Call::Mount) => format!("{volume}: already mounted"),
        ("access_denied", _) => "not authorized".into(),
        (status, _) => status.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_args(arguments: &[&str]) -> Request {
        read(arguments.iter().map(|a| a.to_string()).collect())
    }

    fn volume(name: Option<&str>, id: &str, kind: Option<&str>, state: State) -> Volume {
        Volume {
            name: name.map(String::from),
            id: id.into(),
            kind: kind.map(String::from),
            state,
            removable: true,
        }
    }

    #[test]
    fn reads_a_list_or_a_call() {
        assert_eq!(read_args(&[]), Request::List { json: false });
        assert_eq!(read_args(&["--json"]), Request::List { json: true });
        assert_eq!(
            read_args(&["eject", "STICK"]),
            Request::Call(Call::Eject, "STICK".into())
        );
        assert_eq!(
            read_args(&["mount", "1234-abcd"]),
            Request::Call(Call::Mount, "1234-abcd".into())
        );
        assert_eq!(read_args(&["--help", "eject"]), Request::Help);
    }

    #[test]
    fn refuses_bad_subcommands_and_operands() {
        let invalid = |reason: &str| Request::Invalid(reason.into());
        assert_eq!(read_args(&["list"]), invalid("unknown subcommand list"));
        assert_eq!(read_args(&["eject"]), invalid("eject needs a volume"));
        assert_eq!(
            read_args(&["mount", "a", "b"]),
            invalid("unexpected argument b")
        );
        assert_eq!(
            read_args(&["--json", "eject", "a"]),
            invalid("--json does not go with eject")
        );
    }

    #[test]
    fn names_states() {
        assert_eq!(State::of(true, true), State::Mounted);
        assert_eq!(State::of(false, true), State::Hidden);
        assert_eq!(State::of(false, false), State::Ejected);
    }

    #[test]
    fn writes_aligned_columns() {
        let volumes = [
            volume(
                Some("STICK"),
                "4e564d65-0000-4000-8000-000000000001",
                Some("fat"),
                State::Mounted,
            ),
            volume(None, "1234-abcd", None, State::Ejected),
        ];
        assert_eq!(
            text(&volumes),
            "NAME   ID                                    TYPE  STATE    REMOVABLE\n\
             STICK  4e564d65-0000-4000-8000-000000000001  fat   mounted  yes\n\
             -      1234-abcd                             -     ejected  yes\n"
        );
        assert_eq!(text(&[]), "NAME  ID  TYPE  STATE  REMOVABLE\n");
    }

    #[test]
    fn writes_json() {
        assert_eq!(json(&[]), "[]\n");
        let fixed = Volume {
            removable: false,
            ..volume(None, "1234-abcd", None, State::Hidden)
        };
        assert_eq!(
            json(&[volume(Some("A"), "x", Some("exfat"), State::Mounted), fixed]),
            "[{\"name\":\"A\",\"id\":\"x\",\"type\":\"exfat\",\"state\":\"mounted\",\
             \"removable\":true},{\"name\":null,\"id\":\"1234-abcd\",\"type\":null,\
             \"state\":\"hidden\",\"removable\":false}]\n"
        );
    }

    #[test]
    fn names_failures() {
        assert_eq!(lowercase("BadState"), "bad_state");
        assert_eq!(
            failure(Call::Eject, "STICK", "not_found"),
            "STICK: no such volume"
        );
        assert_eq!(
            failure(Call::Eject, "STICK", "bad_state"),
            "STICK: already ejected"
        );
        assert_eq!(failure(Call::Mount, "x", "bad_state"), "x: already mounted");
        assert_eq!(failure(Call::Mount, "x", "access_denied"), "not authorized");
        assert_eq!(failure(Call::Mount, "x", "timed_out"), "timed_out");
    }
}
