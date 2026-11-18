//! What mkdir takes, the modes of `-m`, octal or symbolic as POSIX
//! `chmod` reads them, and the folders that `-p` creates
//! (MIKSPEC-0098.R37).

use mikros_cli::{Kind, Operand, Opt, Spec};
use std::path::{Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "folder",
        Kind::Folder,
        "the folders to create",
    )],
    options: &[
        Opt::both(
            "parents",
            'p',
            "create the folders above too, and accept folders that exist",
        ),
        Opt::both("verbose", 'v', "name each folder created"),
        Opt::both(
            "mode",
            'm',
            "the mode, octal or symbolic as chmod takes it, such as 755 or u=rwx,go=rx",
        )
        .takes("mode", Kind::Text),
    ],
    ..Spec::new(
        "mkdir",
        "Creates folders. The mode is stored only for compatibility:\n\
         access control lists say who may use a folder.",
    )
};

/// The bits of a mode that a mode sets.
const ALL: u32 = 0o7777;

/// One part of a symbolic mode: whom it changes, empty for everyone as
/// the file creation mask allows, and its actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause {
    who: u32,
    actions: Vec<(char, Perms)>,
}

/// What an action adds, removes or sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perms {
    /// The bits of `r`, `w`, `x`, `s` and `t`, and whether `X` was given.
    Bits(u32, bool),
    /// The rights of the owner, group or others: `u`, `g` or `o`.
    Copy(char),
}

/// A mode of `-m`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Octal(u32),
    Symbolic(Vec<Clause>),
}

impl Mode {
    /// The mode `text`, or `None` when it is not one.
    pub fn parse(text: &str) -> Option<Mode> {
        if !text.is_empty() && text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            let mode = u32::from_str_radix(text, 8).ok()?;
            return (mode <= ALL).then_some(Mode::Octal(mode));
        }
        text.split(',')
            .map(clause)
            .collect::<Option<_>>()
            .map(Mode::Symbolic)
    }

    /// The mode that this one makes of `mode`, for a folder when
    /// `folder`, with `umask` the file creation mask.
    pub fn apply(&self, mode: u32, folder: bool, umask: u32) -> u32 {
        let clauses = match self {
            Mode::Octal(octal) => return *octal,
            Mode::Symbolic(clauses) => clauses,
        };
        let mut mode = mode & ALL;
        for clause in clauses {
            let (who, mask) = match clause.who {
                0 => (ALL, !umask),
                who => (who, ALL),
            };
            for &(op, perms) in &clause.actions {
                let bits = match perms {
                    Perms::Bits(bits, big_x) => {
                        let x = big_x && (folder || mode & 0o111 != 0);
                        bits | if x { 0o111 } else { 0 }
                    }
                    Perms::Copy(from) => {
                        let shift = match from {
                            'u' => 6,
                            'g' => 3,
                            _ => 0,
                        };
                        ((mode >> shift) & 0o7) * 0o111
                    }
                };
                let bits = bits & who & mask;
                mode = match op {
                    '+' => mode | bits,
                    '-' => mode & !bits,
                    _ => (mode & !who) | bits,
                };
            }
        }
        mode
    }
}

/// The clause `text`: `[ugoa]*` then actions of `+`, `-` or `=` with
/// `[rwxXst]*` or one of `u`, `g` and `o`.
fn clause(text: &str) -> Option<Clause> {
    let mut chars = text.chars().peekable();
    let mut who = 0;
    while let Some(&c) = chars.peek() {
        who |= match c {
            'u' => 0o4700,
            'g' => 0o2070,
            'o' => 0o1007,
            'a' => ALL,
            _ => break,
        };
        chars.next();
    }
    let mut actions = Vec::new();
    while let Some(op) = chars.next() {
        if !matches!(op, '+' | '-' | '=') {
            return None;
        }
        let perms = match chars.peek() {
            Some(&c @ ('u' | 'g' | 'o')) => {
                chars.next();
                Perms::Copy(c)
            }
            _ => {
                let (mut bits, mut big_x) = (0, false);
                while let Some(&c) = chars.peek() {
                    bits |= match c {
                        'r' => 0o444,
                        'w' => 0o222,
                        'x' => 0o111,
                        's' => 0o6000,
                        't' => 0o1000,
                        'X' => {
                            big_x = true;
                            0
                        }
                        _ => break,
                    };
                    chars.next();
                }
                Perms::Bits(bits, big_x)
            }
        };
        actions.push((op, perms));
    }
    (!actions.is_empty()).then_some(Clause { who, actions })
}

/// The folders that `-p` creates for `path` when they are missing, from
/// the outermost to `path` itself.
pub fn prefixes(path: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = path
        .ancestors()
        .filter(|a| !a.as_os_str().is_empty() && a.parent().is_some())
        .map(Path::to_path_buf)
        .collect();
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(text: &str, folder: bool) -> u32 {
        Mode::parse(text).unwrap().apply(0o777, folder, 0o022)
    }

    #[test]
    fn reads_octal_modes() {
        assert_eq!(mode("755", true), 0o755);
        assert_eq!(mode("0700", true), 0o700);
        assert_eq!(mode("1777", true), 0o1777);
        assert_eq!(Mode::parse("17777"), None);
        assert_eq!(Mode::parse("8"), None);
    }

    #[test]
    fn reads_symbolic_modes_from_all_rights() {
        assert_eq!(mode("u=rwx,go=rx", true), 0o755);
        assert_eq!(mode("go-w", true), 0o755);
        assert_eq!(mode("a-w", true), 0o555);
        assert_eq!(mode("o=", true), 0o770);
        assert_eq!(mode("g=u", true), 0o777);
        assert_eq!(mode("=rx", true), 0o555);
        assert_eq!(mode("-w", true), 0o577);
        assert_eq!(mode("+t", true), 0o1777);
        assert_eq!(mode("u+s,g-x", true), 0o4767);
        assert_eq!(mode("a=r,u+w", true), 0o644);
        let m = Mode::parse("a=X").unwrap();
        assert_eq!(m.apply(0o644, false, 0), 0);
        assert_eq!(m.apply(0o644, true, 0), 0o111);
        assert_eq!(m.apply(0o744, false, 0), 0o111);
    }

    #[test]
    fn refuses_what_is_not_a_mode() {
        for text in ["", "u", "u+q", "z+r", "u+r,", "+r,,", "u+rg"] {
            assert_eq!(Mode::parse(text), None, "{text}");
        }
    }

    #[test]
    fn lists_the_folders_above() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(prefixes(Path::new("a/b/c")), [p("a"), p("a/b"), p("a/b/c")]);
        assert_eq!(prefixes(Path::new("/x/y")), [p("/x"), p("/x/y")]);
        assert_eq!(prefixes(Path::new("a")), [p("a")]);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/mkdir.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
