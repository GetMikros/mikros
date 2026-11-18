//! What permissions is asked, and what it writes: a header line and one
//! line per grant in aligned columns, a JSON array, or why a call
//! failed.

use mikros_cli::table::{Cell, columns};
use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};

/// The operands of a decision.
const DECIDE: &[Operand] = &[
    Operand::one("app-id", Kind::App, "the app"),
    Operand::one("permission", Kind::Permission, "the permission"),
    Operand::optional("user", Kind::User, "the user, yours when none is named"),
];

pub static COMMAND: Spec = Spec {
    operands: &[Operand::optional(
        "user",
        Kind::User,
        "the user whose decisions to list, yours when none is named",
    )],
    options: &[Opt::long(
        "json",
        "write the grants as a JSON array of objects",
    )],
    subcommands: &[
        Spec {
            operands: DECIDE,
            ..Spec::new("allow", "Allows a permission to an app.")
        },
        Spec {
            operands: DECIDE,
            ..Spec::new("deny", "Denies a permission to an app.")
        },
    ],
    ..Spec::new(
        "permissions",
        "Lists the decisions on permissions of your user, or of user, or\n\
         allows or denies a permission to an app. Listing or denying for\n\
         another user, and allowing, need an administrator's authorization.",
    )
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
}

/// What `run` asks.
pub fn read(run: &Run) -> Request {
    let mut operands = run.operands.clone().into_iter();
    let decision = match run.subcommand() {
        None => {
            return Request::List {
                user: operands.next(),
                json: run.has("--json"),
            };
        }
        Some("allow") => Decision::Allow,
        Some(_) => Decision::Deny,
    };
    Request::Set {
        decision,
        app_id: operands.next().unwrap_or_default(),
        permission: operands.next().unwrap_or_default(),
        user: operands.next(),
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
/// columns aligned to the left, with allowed decisions coloured as
/// allowed and denied ones as denied.
pub fn text(grants: &[Grant], paint: Paint) -> String {
    let mut sorted: Vec<&Grant> = grants.iter().collect();
    sorted.sort_by(|a, b| (&a.app_id, &a.permission).cmp(&(&b.app_id, &b.permission)));
    let rows: Vec<Vec<Cell>> = sorted
        .iter()
        .map(|g| {
            let role = match g.decision {
                "allowed" => Role::Ok,
                _ => Role::Error,
            };
            vec![
                (g.app_id.clone(), None),
                (g.permission.clone(), None),
                (g.decision.into(), Some(role)),
            ]
        })
        .collect();
    let header = ["APP ID", "PERMISSION", "DECISION"];
    columns(&header, &rows, &[false; 3], paint)
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
    use mikros_cli::Parsed;

    fn read_args(arguments: &[&str]) -> Result<Request, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Ok(read(&run)),
            Parsed::Invalid(reason) => Err(reason),
            Parsed::Help(path) => Err(format!("help for {path:?}")),
        }
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
            Ok(Request::List {
                user: None,
                json: false
            })
        );
        assert_eq!(
            read_args(&["bob", "--json"]),
            Ok(Request::List {
                user: Some("bob".into()),
                json: true
            })
        );
        assert_eq!(
            read_args(&["allow", "app.a", "user.camera.use"]),
            Ok(Request::Set {
                decision: Decision::Allow,
                app_id: "app.a".into(),
                permission: "user.camera.use".into(),
                user: None,
            })
        );
        assert_eq!(
            read_args(&["deny", "app.a", "user.camera.use", "bob"]),
            Ok(Request::Set {
                decision: Decision::Deny,
                app_id: "app.a".into(),
                permission: "user.camera.use".into(),
                user: Some("bob".into()),
            })
        );
    }

    #[test]
    fn refuses_bad_operands() {
        let invalid = |reason: &str| Err(reason.into());
        assert_eq!(read_args(&["bob", "x"]), invalid("unexpected argument x"));
        assert_eq!(read_args(&["deny"]), invalid("missing operand <app-id>"));
        assert_eq!(
            read_args(&["allow", "app.a"]),
            invalid("missing operand <permission>")
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
            text(&grants, Paint::never()),
            "APP ID   PERMISSION           DECISION\n\
             app.a    user.camera.use      allowed\n\
             app.a    user.microphone.use  allowed\n\
             app.zed  user.camera.use      denied\n"
        );
        assert_eq!(text(&[], Paint::never()), "APP ID  PERMISSION  DECISION\n");
        let on = Paint::new(Some("always"), false, None);
        assert_eq!(
            text(&grants[..2], on).lines().nth(1),
            Some("app.a    user.microphone.use  \x1b[38;2;17;209;22mallowed\x1b[0m")
        );
        assert_eq!(
            text(&grants[..1], on).lines().nth(1),
            Some("app.zed  user.camera.use  \x1b[38;2;231;76;60mdenied\x1b[0m")
        );
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

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/permissions.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
