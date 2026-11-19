//! What basename takes, and the last component of a name by the steps
//! of POSIX (MIKSPEC-0098.R41).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one("name", Kind::Path, "the name to strip"),
        Operand::any(
            "suffix",
            Kind::Text,
            "a suffix to remove, or with -a or -s more names",
        ),
    ],
    options: &[
        Opt::both("multiple", 'a', "take every operand as a name"),
        Opt::both(
            "suffix",
            's',
            "remove this suffix from every name; implies -a",
        )
        .takes("suffix", Kind::Text),
        Opt::both("zero", 'z', "end each name with NUL, not a newline"),
    ],
    ..Spec::new(
        "basename",
        "Writes each name without the folders before it, and without the\n\
         suffix given, as POSIX defines.",
    )
};

/// The last component of `name`, without `suffix` when the component
/// ends in it and is not just it.
pub fn basename(name: &str, suffix: Option<&str>) -> String {
    if name.is_empty() {
        return String::new();
    }
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    let last = match trimmed.rfind('/') {
        Some(at) => &trimmed[at + 1..],
        None => trimmed,
    };
    match suffix {
        Some(suffix) if !suffix.is_empty() && last != suffix => {
            last.strip_suffix(suffix).unwrap_or(last).into()
        }
        _ => last.into(),
    }
}

/// What the run writes, or why its arguments are invalid.
pub fn output(run: &Run, paint: Paint) -> Result<String, String> {
    let end = match run.has("-z") {
        true => '\0',
        false => '\n',
    };
    let operands = &run.operands;
    let (names, suffix) = match run.has("-a") || run.has("-s") {
        true => (&operands[..], run.value("-s")),
        false if operands.len() > 2 => {
            return Err(format!("unexpected argument {}", operands[2]));
        }
        false => (&operands[..1], operands.get(1).map(String::as_str)),
    };
    let mut text = String::new();
    for name in names {
        let kind = Role::Path(paint.entry_type(name));
        text.push_str(&paint.paint(kind, &basename(name, suffix)));
        text.push(end);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn run(arguments: &[&str]) -> Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    fn out(arguments: &[&str]) -> Result<String, String> {
        output(&run(arguments), Paint::never())
    }

    #[test]
    fn follows_the_examples_of_posix() {
        for (name, base) in [
            ("usr", "usr"),
            ("usr/", "usr"),
            ("", ""),
            ("/", "/"),
            ("//", "/"),
            ("///", "/"),
            ("/usr/", "usr"),
            ("/usr/lib", "lib"),
            ("//usr//lib//", "lib"),
            ("/home//dwc//test", "test"),
        ] {
            assert_eq!(basename(name, None), base, "{name:?}");
        }
    }

    #[test]
    fn removes_a_suffix_that_is_not_the_whole_name() {
        assert_eq!(basename("/usr/src/cmd/cat.c", Some(".c")), "cat");
        assert_eq!(basename("cat.c/", Some(".c")), "cat");
        assert_eq!(basename(".c", Some(".c")), ".c");
        assert_eq!(basename("a.h", Some(".c")), "a.h");
        assert_eq!(basename("/", Some("/")), "/");
    }

    #[test]
    fn takes_one_name_or_many() {
        assert_eq!(out(&["/a/b.txt", ".txt"]), Ok("b\n".into()));
        assert_eq!(out(&["-a", "x/y", "z/", ".txt"]), Ok("y\nz\n.txt\n".into()));
        assert_eq!(out(&["-s", ".c", "a.c", "b/c.c"]), Ok("a\nc\n".into()));
        assert_eq!(out(&["-z", "a/b"]), Ok("b\0".into()));
        assert_eq!(out(&["a", "b", "c"]), Err("unexpected argument c".into()));
        assert!(matches!(
            COMMAND.parse(Vec::new()),
            Parsed::Invalid(why) if why == "missing operand <name>"
        ));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/basename.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
