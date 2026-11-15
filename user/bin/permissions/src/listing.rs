//! What permissions is asked, and what it writes: a header line and one
//! line per grant in aligned columns, a JSON array, or why a call
//! failed.

use mikros_cli::{Arguments, Command, json_string};

pub const COMMAND: Command = Command {
    name: "permissions",
    operands: "[user] | allow|deny <app-id> <permission> [user]",
    most: 4,
    about: "Lists the decisions on permissions of your user, or of user, or\n\
            allows or denies a permission to an app. Listing or denying for\n\
            another user, and allowing, need an administrator's authorization.",
    options: &[("--json", "write the grants as a JSON array of objects")],
    values: &[],
};

/// A decision to store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// What permissions is asked to do; `user` is a user's name, none for
/// the caller's.
#[derive(Debug, PartialEq)]
pub enum Request {
    List {
        user: Option<String>,
        json: bool,
    },
    Set {
        decision: Decision,
        app_id: String,
        permission: String,
        user: Option<String>,
    },
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
    let decision = match operands.first().map(String::as_str) {
        Some("allow") => Decision::Allow,
        Some("deny") => Decision::Deny,
        _ if operands.len() <= 1 => {
            return Request::List {
                user: operands.pop(),
                json: options.contains(&"--json"),
            };
        }
        _ => return Request::Invalid(format!("unknown subcommand {}", operands[0])),
    };
    if !(3..=4).contains(&operands.len()) {
        return Request::Invalid(format!("{} needs an app ID and a permission", operands[0]));
    }
    let user = (operands.len() == 4).then(|| operands.remove(3));
    Request::Set {
        decision,
        permission: operands.remove(2),
        app_id: operands.remove(1),
        user,
    }
}

/// A grant, as permissions shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub user: u32,
    pub app_id: String,
    pub permission: String,
    /// `allowed` or `denied`.
    pub decision: &'static str,
}

/// The header and lines of `grants`, sorted by app ID and permission, in
/// columns aligned to the left.
pub fn text(grants: &[Grant]) -> String {
    let mut sorted: Vec<&Grant> = grants.iter().collect();
    sorted.sort_by(|a, b| (&a.app_id, &a.permission).cmp(&(&b.app_id, &b.permission)));
    let header = ["APP ID", "PERMISSION", "DECISION"];
    let mut rows = vec![header.map(String::from)];
    rows.extend(
        sorted
            .iter()
            .map(|g| [g.app_id.clone(), g.permission.clone(), g.decision.into()]),
    );
    let width = |column: usize| {
        rows.iter()
            .map(|r| r[column].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1)];
    rows.iter()
        .map(|r| {
            format!(
                "{:<w0$}  {:<w1$}  {}\n",
                r[0],
                r[1],
                r[2],
                w0 = widths[0],
                w1 = widths[1],
            )
        })
        .collect()
}

/// `grants`, sorted as `text` sorts them, as a JSON array of objects with
/// the members `user`, `app_id`, `permission` and `decision`, on one
/// line.
pub fn json(grants: &[Grant]) -> String {
    let mut sorted: Vec<&Grant> = grants.iter().collect();
    sorted.sort_by(|a, b| (&a.app_id, &a.permission).cmp(&(&b.app_id, &b.permission)));
    let objects: Vec<String> = sorted
        .iter()
        .map(|g| {
            format!(
                "{{\"user\":{},\"app_id\":{},\"permission\":{},\"decision\":{}}}",
                g.user,
                json_string(&g.app_id),
                json_string(&g.permission),
                json_string(g.decision)
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

/// Why a call on the broker about `permission` failed with the status
/// named `status`, in lowercase with underscores. The broker refuses
/// with `ACCESS_DENIED` when no administrator allowed the call.
pub fn failure(permission: Option<&str>, status: &str) -> String {
    match (permission, status) {
        (Some(p), "not_found") => format!("{p}: no such permission"),
        (Some(p), "invalid_args") => format!("{p}: a system or admin permission"),
        (_, "access_denied") => "not authorized".into(),
        (_, status) => status.into(),
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

    fn grant(app_id: &str, permission: &str, decision: &'static str) -> Grant {
        Grant {
            user: 1000,
            app_id: app_id.into(),
            permission: permission.into(),
            decision,
        }
    }

    #[test]
    fn reads_lists_and_decisions() {
        assert_eq!(
            read_args(&[]),
            Request::List {
                user: None,
                json: false
            }
        );
        assert_eq!(
            read_args(&["bob", "--json"]),
            Request::List {
                user: Some("bob".into()),
                json: true
            }
        );
        assert_eq!(
            read_args(&["allow", "app.a", "user.camera.use"]),
            Request::Set {
                decision: Decision::Allow,
                app_id: "app.a".into(),
                permission: "user.camera.use".into(),
                user: None,
            }
        );
        assert_eq!(
            read_args(&["deny", "app.a", "user.camera.use", "bob"]),
            Request::Set {
                decision: Decision::Deny,
                app_id: "app.a".into(),
                permission: "user.camera.use".into(),
                user: Some("bob".into()),
            }
        );
    }

    #[test]
    fn refuses_bad_operands() {
        let invalid = |reason: &str| Request::Invalid(reason.into());
        assert_eq!(read_args(&["bob", "x"]), invalid("unknown subcommand bob"));
        assert_eq!(
            read_args(&["deny"]),
            invalid("deny needs an app ID and a permission")
        );
        assert_eq!(
            read_args(&["allow", "app.a"]),
            invalid("allow needs an app ID and a permission")
        );
        assert_eq!(
            read_args(&["deny", "a", "b", "c", "d"]),
            invalid("unexpected argument d")
        );
    }

    #[test]
    fn writes_sorted_columns_and_json() {
        let grants = [
            grant("app.zed", "user.camera.use", "denied"),
            grant("app.a", "user.microphone.use", "allowed"),
            grant("app.a", "user.camera.use", "allowed"),
        ];
        assert_eq!(
            text(&grants),
            "APP ID   PERMISSION           DECISION\n\
             app.a    user.camera.use      allowed\n\
             app.a    user.microphone.use  allowed\n\
             app.zed  user.camera.use      denied\n"
        );
        assert_eq!(text(&[]), "APP ID  PERMISSION  DECISION\n");
        assert_eq!(
            json(&grants[..1]),
            "[{\"user\":1000,\"app_id\":\"app.zed\",\"permission\":\"user.camera.use\",\
             \"decision\":\"denied\"}]\n"
        );
        assert_eq!(json(&[]), "[]\n");
    }

    #[test]
    fn names_failures() {
        assert_eq!(lowercase("InvalidArgs"), "invalid_args");
        assert_eq!(
            failure(Some("user.camera.use"), "not_found"),
            "user.camera.use: no such permission"
        );
        assert_eq!(
            failure(Some("admin.time.set"), "invalid_args"),
            "admin.time.set: a system or admin permission"
        );
        assert_eq!(failure(None, "access_denied"), "not authorized");
        assert_eq!(
            failure(Some("user.camera.use"), "access_denied"),
            "not authorized"
        );
        assert_eq!(failure(None, "peer_closed"), "peer_closed");
    }
}
