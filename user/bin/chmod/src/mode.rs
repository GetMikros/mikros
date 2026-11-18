//! What chmod takes, and the modes it sets: octal modes, and symbolic
//! modes as POSIX defines them, applied to a file's stored mode
//! (MIKSPEC-0098.R40).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one(
            "mode",
            Kind::Text,
            "the mode, octal or symbolic, such as 755, u+x or a-w",
        ),
        Operand::any("path", Kind::Path, "the files and folders to change"),
    ],
    options: &[
        Opt::both(
            "recursive",
            'R',
            "change the folders' entries too, leaving links alone",
        ),
        Opt::both("verbose", 'v', "name each file and its old and new mode"),
        Opt::long("reference", "use the mode of <file>, and take no mode")
            .takes("file", Kind::File),
    ],
    // A symbolic mode may start with `-`, as `-w` does.
    dash_operand: Some(|a| a.len() > 1 && Mode::parse(a).is_some()),
    ..Spec::new(
        "chmod",
        "Sets the stored mode of files and folders. Only its execute bits have\n\
         an effect on Mikros; access control lists say who may read and write.",
    )
};

/// What chmod writes once when a change leaves every execute bit as it
/// was.
pub const NOTE: &str = "only the execute bits of a mode have an effect on Mikros; \
                        access control lists decide who may read and write";

/// The permission, set-ID and sticky bits of a mode.
const BITS: u32 = 0o7777;
/// The bits of each class of `who`.
const USER: u32 = 0o4700;
const GROUP: u32 = 0o2070;
const OTHERS: u32 = 0o1007;
const EXECUTE: u32 = 0o111;

/// A mode to set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The whole mode.
    Octal(u32),
    Symbolic(Vec<Clause>),
}

/// One clause of a symbolic mode: whom it is for and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause {
    /// The bits of the classes named, or `None` when none are named, so
    /// that the file creation mask applies.
    who: Option<u32>,
    actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Action {
    op: char,
    perm: Perm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Perm {
    /// Bits of `r`, `w`, `x`, `s` and `t`, and whether `X` was named.
    Bits { bits: u32, search: bool },
    /// The bits that the class at this shift has, `u` 6, `g` 3, `o` 0.
    Copy(u32),
}

impl Mode {
    /// The mode that `text` names, or `None` when it names none.
    pub fn parse(text: &str) -> Option<Mode> {
        if !text.is_empty() && text.len() <= 4 && text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return u32::from_str_radix(text, 8).ok().map(Mode::Octal);
        }
        text.split(',')
            .map(clause)
            .collect::<Option<Vec<Clause>>>()
            .map(Mode::Symbolic)
    }

    /// The mode that a file whose mode is `old` gets; `folder` when it
    /// is a folder, for `X`, and `umask` the file creation mask, for
    /// clauses that name no class.
    pub fn apply(&self, old: u32, folder: bool, umask: u32) -> u32 {
        let clauses = match self {
            Mode::Octal(mode) => return mode & BITS,
            Mode::Symbolic(clauses) => clauses,
        };
        let mut mode = old & BITS;
        for clause in clauses {
            let (who, filter) = match clause.who {
                Some(who) => (who, BITS),
                // The mask filters the permission bits only.
                None => (BITS, !(umask & 0o777)),
            };
            for action in &clause.actions {
                let bits = match action.perm {
                    Perm::Bits { bits, search } => {
                        let x = search && (folder || mode & EXECUTE != 0);
                        bits | if x { EXECUTE } else { 0 }
                    }
                    Perm::Copy(shift) => ((mode >> shift) & 0o7) * 0o111,
                };
                let bits = bits & who & filter;
                match action.op {
                    '+' => mode |= bits,
                    '-' => mode &= !bits,
                    _ => mode = (mode & !who) | bits,
                }
            }
        }
        mode
    }
}

/// The clause `text`: classes, then one or more actions.
fn clause(text: &str) -> Option<Clause> {
    let mut chars = text.chars().peekable();
    let mut who = None;
    while let Some(&c) = chars.peek() {
        let class = match c {
            'u' => USER,
            'g' => GROUP,
            'o' => OTHERS,
            'a' => BITS,
            _ => break,
        };
        who = Some(who.unwrap_or(0) | class);
        chars.next();
    }
    let mut actions = Vec::new();
    while let Some(op) = chars.next() {
        if !matches!(op, '+' | '-' | '=') {
            return None;
        }
        let perm = match chars.peek() {
            Some(&c @ ('u' | 'g' | 'o')) => {
                chars.next();
                Perm::Copy(match c {
                    'u' => 6,
                    'g' => 3,
                    _ => 0,
                })
            }
            _ => {
                let (mut bits, mut search) = (0, false);
                while let Some(&c) = chars.peek() {
                    match c {
                        'r' => bits |= 0o444,
                        'w' => bits |= 0o222,
                        'x' => bits |= EXECUTE,
                        'X' => search = true,
                        's' => bits |= 0o6000,
                        't' => bits |= 0o1000,
                        _ => break,
                    }
                    chars.next();
                }
                Perm::Bits { bits, search }
            }
        };
        actions.push(Action { op, perm });
    }
    match actions.is_empty() {
        true => None,
        false => Some(Clause { who, actions }),
    }
}

/// Whether a change from `old` to `new` leaves every execute bit as it
/// was.
pub fn keeps_execute(old: u32, new: u32) -> bool {
    (old ^ new) & EXECUTE == 0
}

/// What `-v` writes for `path`.
pub fn verbose(path: &str, old: u32, new: u32) -> String {
    match old & BITS == new & BITS {
        true => format!("mode of {path} kept as {:04o}\n", new & BITS),
        false => format!(
            "mode of {path} changed from {:04o} to {:04o}\n",
            old & BITS,
            new & BITS
        ),
    }
}

/// Where the mode comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Mode(Mode),
    /// The mode of this file.
    Reference(String),
}

/// Where the mode comes from and the paths to change, or why the
/// arguments are invalid.
pub fn request(run: &Run) -> Result<(Source, Vec<String>), String> {
    if let Some(file) = run.value("--reference") {
        return Ok((Source::Reference(file.into()), run.operands.clone()));
    }
    let (mode, paths) = run.operands.split_first().ok_or("missing operand <mode>")?;
    if paths.is_empty() {
        return Err("missing operand <path>...".into());
    }
    let mode = Mode::parse(mode).ok_or_else(|| format!("invalid mode {mode}"))?;
    Ok((Source::Mode(mode), paths.to_vec()))
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

    fn apply(mode: &str, old: u32) -> u32 {
        Mode::parse(mode).unwrap().apply(old, false, 0o022)
    }

    #[test]
    fn reads_octal_modes() {
        assert_eq!(Mode::parse("755"), Some(Mode::Octal(0o755)));
        assert_eq!(Mode::parse("0"), Some(Mode::Octal(0)));
        assert_eq!(Mode::parse("4755"), Some(Mode::Octal(0o4755)));
        assert_eq!(Mode::parse("75557"), None);
        assert_eq!(Mode::parse("8"), None);
        assert_eq!(apply("600", 0o4755), 0o600);
    }

    #[test]
    fn takes_modes_that_start_with_a_dash() {
        let run = match COMMAND.parse(["-R", "-w", "f"].map(String::from)) {
            mikros_cli::Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        };
        assert!(run.has("-R"));
        assert_eq!(run.operands, ["-w", "f"]);
    }

    #[test]
    fn applies_symbolic_modes() {
        assert_eq!(apply("u+x", 0o644), 0o744);
        assert_eq!(apply("a-w", 0o664), 0o444);
        assert_eq!(apply("go=", 0o755), 0o700);
        assert_eq!(apply("u=rw,go=r", 0o777), 0o644);
        assert_eq!(apply("ug+rw-x", 0o711), 0o661);
        assert_eq!(apply("g=u", 0o740), 0o770);
        assert_eq!(apply("o+g", 0o650), 0o655);
        assert_eq!(apply("u+s", 0o755), 0o4755);
        assert_eq!(apply("+t", 0o755), 0o1755);
        assert_eq!(apply("a+X", 0o644), 0o644);
        assert_eq!(apply("a+X", 0o744), 0o755);
        let folder = Mode::parse("a+X").unwrap().apply(0o644, true, 0);
        assert_eq!(folder, 0o755);
    }

    #[test]
    fn filters_classless_clauses_by_the_mask() {
        assert_eq!(apply("+w", 0o444), 0o644);
        assert_eq!(apply("+x", 0o644), 0o755);
        assert_eq!(apply("=r", 0o777), 0o444);
        assert_eq!(apply("-w", 0o666), 0o466);
    }

    #[test]
    fn refuses_bad_modes() {
        for bad in ["", "u", "u+x,", "z+x", "u+z", "+x,g", "u*x"] {
            assert_eq!(Mode::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn sees_whether_execute_bits_change() {
        assert!(keeps_execute(0o644, 0o600));
        assert!(keeps_execute(0o755, 0o711));
        assert!(!keeps_execute(0o644, 0o744));
        assert_eq!(
            verbose("a", 0o644, 0o755),
            "mode of a changed from 0644 to 0755\n"
        );
        assert_eq!(verbose("a", 0o644, 0o644), "mode of a kept as 0644\n");
    }

    #[test]
    fn reads_the_mode_or_the_reference() {
        let (source, paths) = request(&run(&["u+x", "a", "b"])).unwrap();
        assert_eq!(source, Source::Mode(Mode::parse("u+x").unwrap()));
        assert_eq!(paths, ["a", "b"]);
        let (source, paths) = request(&run(&["--reference", "r", "a"])).unwrap();
        assert_eq!(source, Source::Reference("r".into()));
        assert_eq!(paths, ["a"]);
        assert_eq!(
            request(&run(&["755"])),
            Err("missing operand <path>...".into())
        );
        assert_eq!(request(&run(&["9x", "a"])), Err("invalid mode 9x".into()));
        assert!(run(&["-Rv", "755", "a"]).has("--recursive"));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/chmod.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
