//! What pkg takes, and what it is asked to do (MIKSPEC-0101.R73).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};

/// An installed package's ID.
const ID: Operand = Operand::one("id", Kind::App, "the ID of the installed package");

pub static COMMAND: Spec = Spec {
    subcommands: &[
        Spec {
            operands: &[Operand::some("file", Kind::File, "the package files")],
            options: &[
                Opt::long(
                    "dependency",
                    "mark the packages as installed only for others",
                ),
                Opt::long(
                    "dev",
                    "keep the headers of libraries, for building against them",
                ),
            ],
            ..Spec::new(
                "install",
                "Installs packages, each after those it needs, asking first.",
            )
        },
        Spec {
            operands: &[
                ID,
                Operand::one("component-id", Kind::Text, "the ID of the component"),
            ],
            ..Spec::new(
                "add",
                "Installs an optional component of an installed package that was left out.",
            )
        },
        Spec {
            operands: &[ID],
            options: &[Opt::long(
                "keep-data",
                "keep the users' data of a removed app",
            )],
            ..Spec::new("remove", "Removes an installed package.")
        },
        Spec {
            options: &[Opt::long(
                "json",
                "write the list as a JSON array of objects",
            )],
            ..Spec::new("list", "Lists the installed packages.")
        },
        Spec {
            operands: &[Operand::one(
                "package",
                Kind::File,
                "a package file, or the ID of an installed package",
            )],
            ..Spec::new("info", "Shows what a package is and what it installs.")
        },
        Spec {
            operands: &[Operand::one("folder", Kind::Folder, "the folder to pack")],
            options: &[
                Opt::short('o', "write the package to this file").takes("file", Kind::File),
                Opt::long(
                    "sysroot",
                    "find needed libraries in this sysroot, on the build machine",
                )
                .takes("path", Kind::Folder),
            ],
            ..Spec::new("pack", "Makes a package from a folder.")
        },
    ],
    ..Spec::new(
        "pkg",
        "Installs, removes, lists, shows and makes .mika packages.",
    )
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
}

/// What `run` asks, or why its arguments are invalid.
pub fn read(run: &Run) -> Result<Request, String> {
    let operands = &run.operands;
    let one = || operands[0].clone();
    Ok(match run.subcommand() {
        None => return Err("a subcommand is missing".into()),
        Some("install") => Request::Install {
            files: operands.clone(),
            dependency: run.has("--dependency"),
            dev: run.has("--dev"),
        },
        Some("add") => Request::Add {
            id: one(),
            component: operands[1].clone(),
        },
        Some("remove") => Request::Remove {
            id: one(),
            keep_data: run.has("--keep-data"),
        },
        Some("list") => Request::List {
            json: run.has("--json"),
        },
        Some("info") => Request::Info(one()),
        Some(_) => Request::Pack {
            folder: one(),
            out: run.value("-o").map(String::from),
            sysroot: run.value("--sysroot").map(String::from),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn r(args: &[&str]) -> Result<Request, String> {
        match COMMAND.parse(args.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => read(&run),
            Parsed::Invalid(reason) => Err(reason),
            Parsed::Help(path) => Err(format!("help for {path:?}")),
        }
    }

    #[test]
    fn reads_each_subcommand() {
        assert_eq!(
            r(&["install", "a.mika", "b.mika", "--dependency", "--dev"]),
            Ok(Request::Install {
                files: vec!["a.mika".into(), "b.mika".into()],
                dependency: true,
                dev: true
            })
        );
        assert_eq!(
            r(&["add", "org.example.x", "org.example.x.part"]),
            Ok(Request::Add {
                id: "org.example.x".into(),
                component: "org.example.x.part".into(),
            })
        );
        assert_eq!(
            r(&["remove", "org.example.x", "--keep-data"]),
            Ok(Request::Remove {
                id: "org.example.x".into(),
                keep_data: true
            })
        );
        assert_eq!(r(&["list", "--json"]), Ok(Request::List { json: true }));
        assert_eq!(
            r(&["info", "org.example.x"]),
            Ok(Request::Info("org.example.x".into()))
        );
        assert_eq!(
            r(&["pack", "dir", "-o", "x.mika"]),
            Ok(Request::Pack {
                folder: "dir".into(),
                out: Some("x.mika".into()),
                sysroot: None,
            })
        );
        assert_eq!(
            r(&["pack", "dir", "--sysroot", "/build/sysroot"]),
            Ok(Request::Pack {
                folder: "dir".into(),
                out: None,
                sysroot: Some("/build/sysroot".into()),
            })
        );
        assert_eq!(
            r(&["pack", "dir", "-ox.mika"])
                .map(|p| matches!(p, Request::Pack { out: Some(_), .. })),
            Ok(true)
        );
        assert_eq!(r(&["--help"]), Err("help for []".into()));
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
            assert!(r(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/pkg.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
