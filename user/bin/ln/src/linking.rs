//! What ln takes, the links that its operands ask for, and relative
//! targets for `-r` (MIKSPEC-0098.R37).

use mikros_cli::{Kind, Operand, Opt, Spec};
use std::path::{Component, Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "target",
        Kind::Path,
        "what to link to, then the link's name or the folder for the links",
    )],
    options: &[
        Opt::both("symbolic", 's', "create symbolic links, not hard links"),
        Opt::both("force", 'f', "remove names that exist"),
        Opt::both(
            "no-dereference",
            'n',
            "treat a name that is a link to a folder as a file",
        ),
        Opt::both(
            "relative",
            'r',
            "with -s, make targets relative to the link's folder",
        ),
        Opt::both("target-directory", 't', "create the links in this folder")
            .takes("folder", Kind::Folder),
        Opt::both(
            "no-target-directory",
            'T',
            "treat the last operand as the link's name, never as a folder",
        ),
        Opt::both("verbose", 'v', "name each link created"),
    ],
    ..Spec::new(
        "ln",
        "Creates links: ln target [name], ln target... folder or ln -t folder target....\n\
         A link without a name goes in the current folder.",
    )
};

/// A link to create: `name`, pointing at `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub name: String,
}

/// Why the operands give no links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wrong {
    /// The arguments are invalid.
    Usage(String),
    /// The last of several operands is not a folder.
    NotFolder(String),
}

/// The links that `operands` ask for, with `into` the folder of `-t` and
/// `no_folder` for `-T`; `is_folder` tells whether a name is a folder to
/// put links in.
pub fn plan(
    operands: &[String],
    into: Option<&str>,
    no_folder: bool,
    is_folder: impl Fn(&str) -> bool,
) -> Result<Vec<Link>, Wrong> {
    let into_folder = |folder: &str, targets: &[String]| {
        targets
            .iter()
            .map(|t| Link {
                target: t.clone(),
                name: join(folder, base(t)),
            })
            .collect()
    };
    match (into, no_folder, operands) {
        (Some(_), true, _) => Err(Wrong::Usage("-t and -T cannot be combined".into())),
        (Some(folder), false, targets) => Ok(into_folder(folder, targets)),
        (None, true, [target, name]) => Ok(vec![Link {
            target: target.clone(),
            name: name.clone(),
        }]),
        (None, _, []) => Err(Wrong::Usage("missing operand <target>...".into())),
        (None, true, [_]) => Err(Wrong::Usage("missing operand <name>".into())),
        (None, true, [_, _, extra, ..]) => {
            Err(Wrong::Usage(format!("unexpected argument {extra}")))
        }
        (None, _, [target]) => Ok(vec![Link {
            target: target.clone(),
            name: base(target).into(),
        }]),
        (None, _, [target, name]) if !is_folder(name) => Ok(vec![Link {
            target: target.clone(),
            name: name.clone(),
        }]),
        (None, _, [targets @ .., folder]) => match is_folder(folder) {
            true => Ok(into_folder(folder, targets)),
            false => Err(Wrong::NotFolder(folder.clone())),
        },
    }
}

/// The last name of `path`, without the slashes at its end.
fn base(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name,
        _ => path,
    }
}

fn join(folder: &str, name: &str) -> String {
    match folder.ends_with('/') {
        true => format!("{folder}{name}"),
        false => format!("{folder}/{name}"),
    }
}

/// `path` made absolute against `cwd` and without `.` and `..`, by its
/// names alone.
pub fn normalized(cwd: &Path, path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in cwd.join(path).components() {
        match component {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// The path that leads from the folder `from` to `to`, both absolute
/// and normalized.
pub fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c);
    }
    match out.as_os_str().is_empty() {
        true => PathBuf::from("."),
        false => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(operands: &[&str], into: Option<&str>, no_folder: bool) -> Result<Vec<Link>, Wrong> {
        let operands: Vec<String> = operands.iter().map(|o| o.to_string()).collect();
        plan(&operands, into, no_folder, |n| n == "dir" || n == "dir/")
    }

    fn link(target: &str, name: &str) -> Link {
        Link {
            target: target.into(),
            name: name.into(),
        }
    }

    #[test]
    fn reads_the_forms() {
        assert_eq!(links(&["/a/b/"], None, false), Ok(vec![link("/a/b/", "b")]));
        assert_eq!(links(&["a", "b"], None, false), Ok(vec![link("a", "b")]));
        assert_eq!(
            links(&["x/a", "dir"], None, false),
            Ok(vec![link("x/a", "dir/a")])
        );
        assert_eq!(links(&["a", "dir"], None, true), Ok(vec![link("a", "dir")]));
        assert_eq!(
            links(&["a", "b", "dir/"], None, false),
            Ok(vec![link("a", "dir/a"), link("b", "dir/b")])
        );
        assert_eq!(
            links(&["a", "b"], Some("t"), false),
            Ok(vec![link("a", "t/a"), link("b", "t/b")])
        );
    }

    #[test]
    fn names_what_is_wrong() {
        assert_eq!(
            links(&["a", "b", "c"], None, false),
            Err(Wrong::NotFolder("c".into()))
        );
        assert!(matches!(
            links(&["a"], Some("t"), true),
            Err(Wrong::Usage(_))
        ));
        assert!(matches!(links(&["a"], None, true), Err(Wrong::Usage(_))));
        assert_eq!(
            links(&["a", "b", "c"], None, true),
            Err(Wrong::Usage("unexpected argument c".into()))
        );
    }

    #[test]
    fn makes_targets_relative() {
        let rel = |from: &str, to: &str| relative(Path::new(from), Path::new(to));
        assert_eq!(rel("/u/a", "/u/a/f"), Path::new("f"));
        assert_eq!(rel("/u/a/b", "/u/c/f"), Path::new("../../c/f"));
        assert_eq!(rel("/", "/x"), Path::new("x"));
        assert_eq!(rel("/x", "/x"), Path::new("."));
        assert_eq!(
            normalized(Path::new("/u"), Path::new("a/../b/./c")),
            Path::new("/u/b/c")
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/ln.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
