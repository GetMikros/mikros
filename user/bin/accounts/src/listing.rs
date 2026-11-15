//! What accounts is asked, and what it writes: a header line and one
//! line per user in aligned columns, a JSON array, or why a call on the
//! accounts service failed.

use mikros_cli::{Arguments, Command, json_string, value};

pub const COMMAND: Command = Command {
    name: "accounts",
    operands: "[add|delete|password|kind|full-name ...]",
    most: 3,
    about: "Lists the users, or changes them:\n\
            \x20 accounts add <name> [--admin] [--full-name <text>]\n\
            \x20 accounts delete <name> [--delete-home]\n\
            \x20 accounts password [name]\n\
            \x20 accounts kind <name> standard|admin\n\
            \x20 accounts full-name <name> <text>\n\
            Changing another user needs an administrator's authorization.",
    options: &[
        ("--json", "write the users as a JSON array of objects"),
        ("--admin", "with add: make an administrator"),
        ("--delete-home", "with delete: delete the home folder too"),
    ],
    values: &[("--full-name", "text", "with add: the full name")],
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
    Help,
    Invalid(String),
}

pub fn read(arguments: Vec<String>) -> Request {
    let (options, values, mut operands) = match COMMAND.parse(arguments) {
        Arguments::Run {
            options,
            values,
            operands,
        } => (options, values, operands),
        Arguments::Help => return Request::Help,
        Arguments::Invalid(reason) => return Request::Invalid(reason),
    };
    let has = |option: &str| options.contains(&option);
    let full_name = value(&values, "--full-name").map(String::from);
    let subcommand = match operands.first() {
        Some(s) => s.clone(),
        None => {
            return Request::List {
                json: has("--json"),
            };
        }
    };
    // The options that the subcommand takes.
    let takes: &[&str] = match subcommand.as_str() {
        "add" => &["--admin", "--full-name"],
        "delete" => &["--delete-home"],
        "password" | "kind" | "full-name" => &[],
        other => return Request::Invalid(format!("unknown subcommand {other}")),
    };
    let given = options
        .iter()
        .copied()
        .chain(values.iter().map(|(o, _)| *o));
    if let Some(option) = given.into_iter().find(|o| !takes.contains(o)) {
        return Request::Invalid(format!("{option} cannot go with {subcommand}"));
    }
    let count = operands.len() - 1;
    let wanted = match subcommand.as_str() {
        "password" => 0..=1,
        "kind" | "full-name" => 2..=2,
        _ => 1..=1,
    };
    if !wanted.contains(&count) {
        return Request::Invalid(match count < *wanted.start() {
            true => format!("{subcommand} is missing an operand"),
            false => format!("unexpected argument {}", operands[wanted.end() + 1]),
        });
    }
    operands.remove(0);
    let mut operands = operands.into_iter();
    let mut next = || operands.next().unwrap_or_default();
    if full_name.as_deref() == Some("") {
        return Request::Invalid("the full name is empty".into());
    }
    match subcommand.as_str() {
        "add" => {
            let name = next();
            if !valid_name(&name) {
                return Request::Invalid(format!("invalid user name {name}"));
            }
            Request::Add {
                name,
                admin: has("--admin"),
                full_name,
            }
        }
        "delete" => Request::Delete {
            name: next(),
            delete_home: has("--delete-home"),
        },
        "password" => Request::Password {
            name: (count == 1).then(next),
        },
        "kind" => {
            let name = next();
            match next().as_str() {
                "standard" => Request::Kind { name, admin: false },
                "admin" => Request::Kind { name, admin: true },
                other => Request::Invalid(format!("unknown kind {other}")),
            }
        }
        _ => {
            let name = next();
            match next() {
                text if text.is_empty() => Request::Invalid("the full name is empty".into()),
                text => Request::FullName { name, text },
            }
        }
    }
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
pub fn text(users: &[User]) -> String {
    let header = ["ID", "NAME", "KIND", "FULL NAME"];
    let mut rows = vec![header.map(String::from)];
    rows.extend(users.iter().map(|u| {
        [
            u.id.to_string(),
            u.name.clone(),
            u.kind.into(),
            u.full_name.clone(),
        ]
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
                "{:>w0$}  {:<w1$}  {:<w2$}  {}\n",
                r[0],
                r[1],
                r[2],
                r[3],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
            )
        })
        .collect()
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

    fn read_args(arguments: &[&str]) -> Request {
        read(arguments.iter().map(|a| a.to_string()).collect())
    }

    #[test]
    fn reads_every_subcommand() {
        assert_eq!(read_args(&["--json"]), Request::List { json: true });
        assert_eq!(
            read_args(&["add", "bob", "--admin", "--full-name", "Bob B"]),
            Request::Add {
                name: "bob".into(),
                admin: true,
                full_name: Some("Bob B".into()),
            }
        );
        assert_eq!(
            read_args(&["add", "bob"]),
            Request::Add {
                name: "bob".into(),
                admin: false,
                full_name: None,
            }
        );
        assert_eq!(
            read_args(&["delete", "bob", "--delete-home"]),
            Request::Delete {
                name: "bob".into(),
                delete_home: true,
            }
        );
        assert_eq!(read_args(&["password"]), Request::Password { name: None });
        assert_eq!(
            read_args(&["password", "bob"]),
            Request::Password {
                name: Some("bob".into())
            }
        );
        assert_eq!(
            read_args(&["kind", "bob", "admin"]),
            Request::Kind {
                name: "bob".into(),
                admin: true,
            }
        );
        assert_eq!(
            read_args(&["full-name", "bob", "Bob B"]),
            Request::FullName {
                name: "bob".into(),
                text: "Bob B".into(),
            }
        );
    }

    #[test]
    fn refuses_bad_subcommands_operands_and_options() {
        let invalid = |reason: &str| Request::Invalid(reason.into());
        assert_eq!(
            read_args(&["rename", "a"]),
            invalid("unknown subcommand rename")
        );
        assert_eq!(read_args(&["add"]), invalid("add is missing an operand"));
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
            invalid("--admin cannot go with delete")
        );
        assert_eq!(
            read_args(&["kind", "a", "admin", "--full-name", "x"]),
            invalid("--full-name cannot go with kind")
        );
        assert_eq!(
            read_args(&["add", "a", "--json"]),
            invalid("--json cannot go with add")
        );
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
            text(&users),
            "  ID  NAME  KIND      FULL NAME\n\
             1000  ada   admin     Ada Lovelace\n\
             1001  bob   standard  Bob\n"
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
}
