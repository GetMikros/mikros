//! What dirname takes, and the folder part of a name by the steps of
//! POSIX (MIKSPEC-0098.R41).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "name",
        Kind::Path,
        "the names whose folders to write",
    )],
    options: &[Opt::both(
        "zero",
        'z',
        "end each folder with NUL, not a newline",
    )],
    ..Spec::new(
        "dirname",
        "Writes each name without its last component, as POSIX defines.",
    )
};

/// The folder part of `name`: `/` for a name of slashes or one right
/// below `/`, and `.` for a name without a slash.
pub fn dirname(name: &str) -> String {
    if !name.is_empty() && name.trim_start_matches('/').is_empty() {
        return "/".into();
    }
    let trimmed = name.trim_end_matches('/');
    let Some(at) = trimmed.rfind('/') else {
        return ".".into();
    };
    let folder = trimmed[..=at].trim_end_matches('/');
    match folder.is_empty() {
        true => "/".into(),
        false => folder.into(),
    }
}

/// What the run writes.
pub fn output(run: &Run, paint: Paint) -> String {
    let end = match run.has("-z") {
        true => '\0',
        false => '\n',
    };
    let mut text = String::new();
    for name in &run.operands {
        text.push_str(&paint.paint(Role::Path, &dirname(name)));
        text.push(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn out(arguments: &[&str]) -> String {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => output(&run, Paint::never()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn follows_the_examples_of_posix() {
        for (name, folder) in [
            ("/", "/"),
            ("//", "/"),
            ("/a/b/", "/a"),
            ("//a//b//", "//a"),
            ("a", "."),
            ("", "."),
            ("/a", "/"),
            ("/a/b", "/a"),
            ("a/b", "a"),
            ("/usr/lib", "/usr"),
            ("/usr/", "/"),
            ("usr", "."),
            (".", "."),
            ("..", "."),
            ("a//b", "a"),
            ("///a", "/"),
        ] {
            assert_eq!(dirname(name), folder, "{name:?}");
        }
    }

    #[test]
    fn takes_many_names() {
        assert_eq!(out(&["/a/b", "c"]), "/a\n.\n");
        assert_eq!(out(&["-z", "a/b"]), "a\0");
        assert!(matches!(
            COMMAND.parse(Vec::new()),
            Parsed::Invalid(why) if why == "missing operand <name>..."
        ));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/dirname.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
