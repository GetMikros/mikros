//! What accounts is asked, and what it writes: a header line and one
//! line per user in aligned columns, a JSON array, or why a call on the
//! accounts service failed.

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};

/// The user that a subcommand changes.
const USER: Operand = Operand::one("name", Kind::User, "the user's name");

pub static COMMAND: Spec = Spec {
    options: &[Opt::long(
        "json",
        "write the users as a JSON array of objects",
    )],
    subcommands: &[
        Spec {
            operands: &[Operand::one("name", Kind::Text, "the new user's name")],
            options: &[
                Opt::long("admin", "make an administrator"),
                Opt::long("full-name", "the full name, the name when none is given")
                    .takes("text", Kind::Text),
            ],
            ..Spec::new("add", "Adds a user, asking for the new password twice.")
        },
        Spec {
            operands: &[USER],
            options: &[Opt::long("delete-home", "delete the home folder too")],
            ..Spec::new("delete", "Deletes a user.")
        },
        Spec {
            operands: &[Operand::optional(
                "name",
                Kind::User,
                "the user's name, yours when none is given",
            )],
            ..Spec::new(
                "password",
                "Sets a user's password, asking for your current one when it is yours.",
            )
        },
        Spec {
            operands: &[
                USER,
                Operand::one(
                    "kind",
                    Kind::Choice(&["standard", "admin"]),
                    "standard or admin",
                ),
            ],
            ..Spec::new("kind", "Makes a user a standard user or an administrator.")
        },
        Spec {
            operands: &[USER, Operand::one("text", Kind::Text, "the full name")],
            ..Spec::new("full-name", "Sets a user's full name.")
        },
    ],
    ..Spec::new(
        "accounts",
        "Lists the users, or adds, deletes or changes one. Changing another\n\
         user needs an administrator's authorization.",
    )
};

/// What accounts is asked to do. Users are named.
#[derive(Debug, PartialEq)]
pub enum Request {
    List {
        json: bool,
    },
    Add {
        name: String,
        admin: bool,
        full_name: Option<String>,
    },
    Delete {
        name: String,
        delete_home: bool,
    },
    /// The caller's own password without a name.
    Password {
        name: Option<String>,
    },
    Kind {
        name: String,
        admin: bool,
    },
    FullName {
        name: String,
        text: String,
    },
}

/// What `run` asks, or why its arguments are invalid.
pub fn read(run: &Run) -> Result<Request, String> {
    let Some(subcommand) = run.subcommand() else {
        return Ok(Request::List {
            json: run.has("--json"),
        });
    };
    if run.has("--json") {
        return Err(format!("--json cannot go with {subcommand}"));
    }
    let full_name = run.value("--full-name").map(String::from);
    if full_name.as_deref() == Some("") {
        return Err("the full name is empty".into());
    }
    let mut operands = run.operands.clone().into_iter();
    let mut next = || operands.next().unwrap_or_default();
    Ok(match subcommand {
        "add" => {
            let name = next();
            if !valid_name(&name) {
                return Err(format!("invalid user name {name}"));
            }
            Request::Add {
                name,
                admin: run.has("--admin"),
                full_name,
            }
        }
        "delete" => Request::Delete {
            name: next(),
            delete_home: run.has("--delete-home"),
        },
        "password" => Request::Password {
            name: run.operands.first().cloned(),
        },
        "kind" => {
            let name = next();
            match next().as_str() {
                "standard" => Request::Kind { name, admin: false },
                "admin" => Request::Kind { name, admin: true },
                other => return Err(format!("unknown kind {other}")),
            }
        }
        _ => {
            let name = next();
            match next() {
                text if text.is_empty() => return Err("the full name is empty".into()),
                text => Request::FullName { name, text },
            }
        }
    })
}

/// The longest user name.
const NAME_MAX: usize = 32;

/// Whether `name` is a valid user name: a lowercase letter, then at most
/// 31 lowercase letters, digits and hyphens.
fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && name.len() <= NAME_MAX
        && bytes.all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// A user, as accounts shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub id: u32,
    pub name: String,
    pub full_name: String,
    /// `standard` or `admin`.
    pub kind: &'static str,
}

/// The header and lines of `users`: the ID aligned to the right, the
/// name and kind to the left, and the full name.
pub fn text(users: &[User], paint: Paint) -> String {
    let header = ["ID", "NAME", "KIND", "FULL NAME"];
    let rows: Vec<Vec<Cell>> = users
        .iter()
        .map(|u| {
            vec![
                (u.id.to_string(), Some(Role::Number)),
                (u.name.clone(), None),
                (u.kind.into(), None),
                (u.full_name.clone(), Some(Role::String)),
            ]
        })
        .collect();
    columns(&header, &rows, &[true, false, false, false], paint)
}

/// A cell of a table: its text, and the role it is coloured in.
type Cell = (String, Option<Role>);

/// The header and `rows` in columns two spaces apart, the header in the
/// secondary colour, each column as wide as its widest text without
/// colours, aligned to the right where `right` says, the last one not
/// padded.
fn columns(header: &[&str], rows: &[Vec<Cell>], right: &[bool], paint: Paint) -> String {
    let header: Vec<Cell> = header
        .iter()
        .map(|h| (h.to_string(), Some(Role::Secondary)))
        .collect();
    let all: Vec<&Vec<Cell>> = std::iter::once(&header).chain(rows).collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|c| {
            all.iter()
                .map(|r| r[c].0.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut text = String::new();
    for row in all {
        for (c, (cell, role)) in row.iter().enumerate() {
            let pad = " ".repeat(widths[c] - cell.chars().count());
            let shown = match role {
                Some(role) => paint.paint(*role, cell),
                None => cell.clone(),
            };
            if c > 0 {
                text.push_str("  ");
            }
            match (right[c], c + 1 == row.len()) {
                (true, _) => text.push_str(&(pad + &shown)),
                (false, true) => text.push_str(&shown),
                (false, false) => text.push_str(&(shown + &pad)),
            }
        }
        text.push('\n');
    }
    text
}

/// `users` as a JSON array of objects with the members `id`, `name`,
/// `full_name` and `kind`, on one line.
pub fn json(users: &[User]) -> String {
    let objects: Vec<String> = users
        .iter()
        .map(|u| {
            format!(
                "{{\"id\":{},\"name\":{},\"full_name\":{},\"kind\":{}}}",
                u.id,
                json_string(&u.name),
                json_string(&u.full_name),
                json_string(u.kind)
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

/// Why a call about the user `name` failed with the status named
/// `status`, in lowercase with underscores; `own_password` when it
/// changed the caller's own password.
pub fn failure(name: &str, status: &str, own_password: bool) -> String {
    match status {
        "not_found" => format!("{name}: no such user"),
        "already_exists" => format!("{name}: already exists"),
        "bad_state" => format!("{name}: the last administrator"),
        "access_denied" if own_password => "wrong password".into(),
        "access_denied" => "not authorized".into(),
        status => status.into(),
    }
}

/// A name of a status as MIDL writes it in Rust, such as `NotFound`, in
/// lowercase with underscores: `not_found`.
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

    #[test]
    fn reads_every_subcommand() {
        assert_eq!(read_args(&["--json"]), Ok(Request::List { json: true }));
        assert_eq!(
            read_args(&["add", "bob", "--admin", "--full-name", "Bob B"]),
            Ok(Request::Add {
                name: "bob".into(),
                admin: true,
                full_name: Some("Bob B".into()),
            })
        );
        assert_eq!(
            read_args(&["add", "bob"]),
            Ok(Request::Add {
                name: "bob".into(),
                admin: false,
                full_name: None,
            })
        );
        assert_eq!(
            read_args(&["delete", "bob", "--delete-home"]),
            Ok(Request::Delete {
                name: "bob".into(),
                delete_home: true,
            })
        );
        assert_eq!(
            read_args(&["password"]),
            Ok(Request::Password { name: None })
        );
        assert_eq!(
            read_args(&["password", "bob"]),
            Ok(Request::Password {
                name: Some("bob".into())
            })
        );
        assert_eq!(
            read_args(&["kind", "bob", "admin"]),
            Ok(Request::Kind {
                name: "bob".into(),
                admin: true,
            })
        );
        assert_eq!(
            read_args(&["full-name", "bob", "Bob B"]),
            Ok(Request::FullName {
                name: "bob".into(),
                text: "Bob B".into(),
            })
        );
    }

    #[test]
    fn refuses_bad_subcommands_operands_and_options() {
        let invalid = |reason: &str| Err(reason.into());
        assert_eq!(
            read_args(&["rename", "a"]),
            invalid("unexpected argument rename")
        );
        assert_eq!(read_args(&["add"]), invalid("missing operand <name>"));
        assert_eq!(read_args(&["kind", "a"]), invalid("missing operand <kind>"));
        assert_eq!(
            read_args(&["delete", "a", "b"]),
            invalid("unexpected argument b")
        );
        assert_eq!(
            read_args(&["password", "a", "b"]),
            invalid("unexpected argument b")
        );
        assert_eq!(
            read_args(&["kind", "a", "root"]),
            invalid("unknown kind root")
        );
        assert_eq!(
            read_args(&["delete", "a", "--admin"]),
            invalid("unknown option --admin")
        );
        assert_eq!(
            read_args(&["kind", "a", "admin", "--full-name", "x"]),
            invalid("unknown option --full-name")
        );
        assert_eq!(
            read_args(&["add", "a", "--json"]),
            invalid("--json cannot go with add")
        );
        assert_eq!(
            read_args(&["--json", "password"]),
            invalid("--json cannot go with password")
        );
        assert_eq!(read_args(&["--admin"]), invalid("unknown option --admin"));
        assert_eq!(read_args(&["add", "Bob"]), invalid("invalid user name Bob"));
        assert_eq!(read_args(&["add", "9a"]), invalid("invalid user name 9a"));
        let long = "a".repeat(33);
        assert_eq!(
            read_args(&["add", &long]),
            invalid(&format!("invalid user name {long}"))
        );
        assert_eq!(
            read_args(&["add", "bob", "--full-name", ""]),
            invalid("the full name is empty")
        );
        assert_eq!(
            read_args(&["full-name", "bob", ""]),
            invalid("the full name is empty")
        );
    }

    #[test]
    fn writes_columns_and_json() {
        let users = [
            User {
                id: 1000,
                name: "ada".into(),
                full_name: "Ada Lovelace".into(),
                kind: "admin",
            },
            User {
                id: 1001,
                name: "bob".into(),
                full_name: "Bob".into(),
                kind: "standard",
            },
        ];
        assert_eq!(
            text(&users, Paint::never()),
            "  ID  NAME  KIND      FULL NAME\n\
             1000  ada   admin     Ada Lovelace\n\
             1001  bob   standard  Bob\n"
        );
        let on = Paint::new(Some("always"), false, None);
        assert_eq!(
            text(&users[1..], on).lines().nth(1),
            Some(
                "\x1b[38;2;21;193;160m1001\x1b[0m  bob   standard  \x1b[38;2;255;134;26mBob\x1b[0m"
            )
        );
        assert_eq!(
            json(&users[..1]),
            "[{\"id\":1000,\"name\":\"ada\",\"full_name\":\"Ada Lovelace\",\"kind\":\"admin\"}]\n"
        );
        assert_eq!(json(&[]), "[]\n");
    }

    #[test]
    fn names_failures() {
        assert_eq!(failure("bob", "not_found", false), "bob: no such user");
        assert_eq!(
            failure("bob", "already_exists", false),
            "bob: already exists"
        );
        assert_eq!(
            failure("ada", "bad_state", false),
            "ada: the last administrator"
        );
        assert_eq!(failure("ada", "access_denied", true), "wrong password");
        assert_eq!(failure("bob", "access_denied", false), "not authorized");
        assert_eq!(failure("bob", "invalid_args", false), "invalid_args");
        assert_eq!(lowercase("NoResources"), "no_resources");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/accounts.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
