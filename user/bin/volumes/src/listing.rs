//! What volumes is asked, and what it writes: a header line and one line
//! per volume in aligned columns, a JSON array, or why a call on a volume
//! failed.

use mikros_cli::table::{Cell, columns};
use mikros_cli::{EntryType, Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};

pub static COMMAND: Spec = Spec {
    options: &[Opt::long(
        "json",
        "write the volumes as a JSON array of objects",
    )],
    subcommands: &[
        Spec {
            operands: &[Operand::one(
                "volume",
                Kind::Volume,
                "the name in /volumes or the ID of the volume",
            )],
            ..Spec::new("eject", "Ejects a volume.")
        },
        Spec {
            operands: &[Operand::one("id", Kind::Volume, "the ID of the volume")],
            ..Spec::new("mount", "Mounts an ejected volume.")
        },
    ],
    ..Spec::new(
        "volumes",
        "Lists the volumes with their names, IDs, file systems and states,\n\
         or ejects one by its name or ID, or mounts one by its ID.",
    )
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
}

/// What `run` asks, or why its arguments are invalid.
pub fn read(run: &Run) -> Result<Request, String> {
    let json = run.has("--json");
    let call = match run.subcommand() {
        None => return Ok(Request::List { json }),
        Some("eject") => Call::Eject,
        Some(_) => Call::Mount,
    };
    if json {
        let subcommand = run.subcommand().unwrap_or_default();
        return Err(format!("--json does not go with {subcommand}"));
    }
    Ok(Request::Call(call, run.operands[0].clone()))
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
pub fn text(volumes: &[Volume], paint: Paint) -> String {
    let header = ["NAME", "ID", "TYPE", "STATE", "REMOVABLE"];
    let or_none = |text: &Option<String>, role: Option<Role>| match text {
        Some(text) => (text.clone(), role),
        None => ("-".into(), Some(Role::Secondary)),
    };
    let rows: Vec<Vec<Cell>> = volumes
        .iter()
        .map(|v| {
            let state = (v.state == State::Mounted).then_some(Role::Ok);
            let removable = match v.removable {
                true => "yes",
                false => "no",
            };
            vec![
                or_none(&v.name, Some(Role::Path(EntryType::Folder))),
                (v.id.clone(), Some(Role::Number)),
                or_none(&v.kind, None),
                (v.state.name().into(), state),
                (removable.into(), None),
            ]
        })
        .collect();
    columns(&header, &rows, &[false; 5], paint)
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
    use mikros_cli::Parsed;

    fn read_args(arguments: &[&str]) -> Result<Request, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => read(&run),
            Parsed::Invalid(reason) => Err(reason),
            Parsed::Help(path) => Err(format!("help for {path:?}")),
        }
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
        assert_eq!(read_args(&[]), Ok(Request::List { json: false }));
        assert_eq!(read_args(&["--json"]), Ok(Request::List { json: true }));
        assert_eq!(
            read_args(&["eject", "STICK"]),
            Ok(Request::Call(Call::Eject, "STICK".into()))
        );
        assert_eq!(
            read_args(&["mount", "1234-abcd"]),
            Ok(Request::Call(Call::Mount, "1234-abcd".into()))
        );
        assert_eq!(
            read_args(&["--help", "eject"]),
            Err("help for [\"eject\"]".into())
        );
    }

    #[test]
    fn refuses_bad_subcommands_and_operands() {
        let invalid = |reason: &str| Err(reason.into());
        assert_eq!(read_args(&["list"]), invalid("unexpected argument list"));
        assert_eq!(read_args(&["eject"]), invalid("missing operand <volume>"));
        assert_eq!(
            read_args(&["mount", "a", "b"]),
            invalid("unexpected argument b")
        );
        assert_eq!(
            read_args(&["--json", "eject", "a"]),
            invalid("--json does not go with eject")
        );
        assert_eq!(
            read_args(&["mount", "a", "--json"]),
            invalid("--json does not go with mount")
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
            text(&volumes, Paint::never()),
            "NAME   ID                                    TYPE  STATE    REMOVABLE\n\
             STICK  4e564d65-0000-4000-8000-000000000001  fat   mounted  yes\n\
             -      1234-abcd                             -     ejected  yes\n"
        );
        assert_eq!(
            text(&[], Paint::never()),
            "NAME  ID  TYPE  STATE  REMOVABLE\n"
        );
        let on = Paint::new(Some("always"), false, None);
        let paint = |sgr: &str, t: &str| format!("\x1b[{sgr}m{t}\x1b[0m");
        assert_eq!(
            text(&volumes, on).lines().nth(2).unwrap(),
            format!(
                "{}      1234-abcd                             {}     ejected  yes",
                paint("90", "-"),
                paint("90", "-")
            )
        );
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

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/volumes.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
