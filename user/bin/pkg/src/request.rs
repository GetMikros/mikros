//! What pkg is asked to do (MIKSPEC-0101.R73).

use mikros_cli::{Arguments, Command, value};

pub const COMMAND: Command = Command {
    name: "pkg",
    operands: "install <file>...|add <id> <component-id>|remove <id>|list|info <file|id>|pack <folder>",
    most: usize::MAX,
    about: "Installs, removes, lists, shows and makes .mika packages.",
    options: &[
        (
            "--dependency",
            "mark the packages as installed only for others",
        ),
        (
            "--dev",
            "keep the headers of libraries, for building against them",
        ),
        ("--keep-data", "keep the users' data of a removed app"),
        ("--json", "write the list as a JSON array of objects"),
    ],
    values: &[
        ("-o", "file", "write the package to this file"),
        (
            "--sysroot",
            "path",
            "find needed libraries in this sysroot, on the build machine",
        ),
    ],
};

#[derive(Debug, PartialEq)]
pub enum Request {
    Install {
        files: Vec<String>,
        dependency: bool,
        dev: bool,
    },
    /// A left-out optional component of an installed package, by its ID.
    Add {
        id: String,
        component: String,
    },
    Remove {
        id: String,
        keep_data: bool,
    },
    List {
        json: bool,
    },
    Info(String),
    Pack {
        folder: String,
        out: Option<String>,
        sysroot: Option<String>,
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
    if operands.is_empty() {
        return Request::Invalid("a subcommand is missing".into());
    }
    let subcommand = operands.remove(0);
    // The options that each subcommand takes.
    let allowed: &[&str] = match subcommand.as_str() {
        "install" => &["--dependency", "--dev"],
        "add" => &[],
        "remove" => &["--keep-data"],
        "list" => &["--json"],
        "info" | "pack" => &[],
        other => return Request::Invalid(format!("unknown subcommand {other}")),
    };
    if let Some(option) = options.iter().find(|o| !allowed.contains(o)) {
        return Request::Invalid(format!("{option} does not go with {subcommand}"));
    }
    let out = value(&values, "-o").map(String::from);
    if out.is_some() && subcommand != "pack" {
        return Request::Invalid(format!("-o does not go with {subcommand}"));
    }
    let sysroot = value(&values, "--sysroot").map(String::from);
    if sysroot.is_some() && subcommand != "pack" {
        return Request::Invalid(format!("--sysroot does not go with {subcommand}"));
    }
    let one = |what: &str| match operands.len() {
        1 => Ok(operands[0].clone()),
        0 => Err(Request::Invalid(format!("{subcommand} needs {what}"))),
        _ => Err(Request::Invalid(format!("{subcommand} takes one {what}"))),
    };
    match subcommand.as_str() {
        "install" => {
            if operands.is_empty() {
                return Request::Invalid("install needs a package file".into());
            }
            Request::Install {
                files: operands,
                dependency: options.contains(&"--dependency"),
                dev: options.contains(&"--dev"),
            }
        }
        "add" => match &operands[..] {
            [id, component] => Request::Add {
                id: id.clone(),
                component: component.clone(),
            },
            _ => Request::Invalid("add takes a package ID and a component's ID".into()),
        },
        "remove" => match one("a package ID") {
            Ok(id) => Request::Remove {
                id,
                keep_data: options.contains(&"--keep-data"),
            },
            Err(e) => e,
        },
        "list" if operands.is_empty() => Request::List {
            json: options.contains(&"--json"),
        },
        "list" => Request::Invalid("list takes no operands".into()),
        "info" => match one("a package file or ID") {
            Ok(target) => Request::Info(target),
            Err(e) => e,
        },
        _ => match one("a folder") {
            Ok(folder) => Request::Pack {
                folder,
                out,
                sysroot,
            },
            Err(e) => e,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(args: &[&str]) -> Request {
        read(args.iter().map(|a| a.to_string()).collect())
    }

    #[test]
    fn reads_each_subcommand() {
        assert_eq!(
            r(&["install", "a.mika", "b.mika", "--dependency", "--dev"]),
            Request::Install {
                files: vec!["a.mika".into(), "b.mika".into()],
                dependency: true,
                dev: true
            }
        );
        assert_eq!(
            r(&["add", "org.example.x", "org.example.x.part"]),
            Request::Add {
                id: "org.example.x".into(),
                component: "org.example.x.part".into(),
            }
        );
        assert_eq!(
            r(&["remove", "org.example.x", "--keep-data"]),
            Request::Remove {
                id: "org.example.x".into(),
                keep_data: true
            }
        );
        assert_eq!(r(&["list", "--json"]), Request::List { json: true });
        assert_eq!(
            r(&["info", "org.example.x"]),
            Request::Info("org.example.x".into())
        );
        assert_eq!(
            r(&["pack", "dir", "-o", "x.mika"]),
            Request::Pack {
                folder: "dir".into(),
                out: Some("x.mika".into()),
                sysroot: None,
            }
        );
        assert_eq!(
            r(&["pack", "dir", "--sysroot", "/build/sysroot"]),
            Request::Pack {
                folder: "dir".into(),
                out: None,
                sysroot: Some("/build/sysroot".into()),
            }
        );
        assert_eq!(r(&["--help"]), Request::Help);
    }

    #[test]
    fn refuses_what_does_not_fit() {
        for args in [
            &[][..],
            &["frobnicate"],
            &["install"],
            &["install", "a", "--global"],
            &["install", "a", "--local"],
            &["remove", "--json", "x"],
            &["remove"],
            &["remove", "a", "b"],
            &["list", "x"],
            &["info", "a", "-o", "b"],
            &["list", "--sysroot", "s"],
            &["pack"],
            &["add", "org.example.x"],
            &["add", "a", "b", "c"],
            &["add", "a", "b", "--json"],
        ] {
            assert!(matches!(r(args), Request::Invalid(_)), "{args:?}");
        }
    }
}
