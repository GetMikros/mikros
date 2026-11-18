//! What cat takes, and how it numbers, squeezes and shows the lines it
//! copies (MIKSPEC-0098.R36).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to copy, - for standard input",
    )],
    options: &[
        Opt::both("number", 'n', "number every line"),
        Opt::both(
            "number-nonblank",
            'b',
            "number the lines that are not empty, over -n",
        ),
        Opt::both("squeeze-blank", 's', "write one empty line for several"),
        Opt::both(
            "show-all",
            'A',
            "as -v, with $ at the end of each line and tabs as ^I",
        ),
        Opt::both(
            "show-nonprinting",
            'v',
            "show control characters as ^ and bytes above 127 as M-",
        ),
    ],
    ..Spec::new(
        "cat",
        "Copies the files named, or standard input, to standard output.",
    )
};

/// How lines are shown, from the options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Show {
    pub number: bool,
    pub nonblank: bool,
    pub squeeze: bool,
    pub ends: bool,
    pub tabs: bool,
    pub nonprinting: bool,
}

impl Show {
    pub fn of(run: &Run) -> Show {
        let all = run.has("-A");
        Show {
            number: run.has("-n"),
            nonblank: run.has("-b"),
            squeeze: run.has("-s"),
            ends: all,
            tabs: all,
            nonprinting: all || run.has("-v"),
        }
    }

    /// Whether the contents are copied as they are.
    pub fn plain(self) -> bool {
        self == Show::default()
    }
}

/// The lines being copied, across every file, as numbering and squeezing
/// go on from one file to the next.
pub struct Lines {
    show: Show,
    /// The number of the last line numbered.
    number: u64,
    /// Whether the next byte starts a line.
    at_start: bool,
    /// The empty lines read in a row.
    blanks: u64,
}

impl Lines {
    pub fn new(show: Show) -> Lines {
        Lines {
            show,
            number: 0,
            at_start: true,
            blanks: 0,
        }
    }

    /// Appends `input`, as shown, to `out`.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let show = self.show;
        for &byte in input {
            if self.at_start {
                if byte == b'\n' {
                    self.blanks += 1;
                    if show.squeeze && self.blanks > 1 {
                        continue;
                    }
                    if show.number && !show.nonblank {
                        self.prefix(out);
                    }
                    if show.ends {
                        out.push(b'$');
                    }
                    out.push(b'\n');
                    continue;
                }
                self.blanks = 0;
                self.at_start = false;
                if show.number || show.nonblank {
                    self.prefix(out);
                }
            }
            if byte == b'\n' {
                if show.ends {
                    out.push(b'$');
                }
                out.push(b'\n');
                self.at_start = true;
                continue;
            }
            shown(byte, show, out);
        }
    }

    /// The next line's number, right-aligned in six columns, and a tab.
    fn prefix(&mut self, out: &mut Vec<u8>) {
        self.number += 1;
        out.extend_from_slice(format!("{:>6}\t", self.number).as_bytes());
    }
}

/// Appends `byte`, not a line feed, as `show` has it shown.
fn shown(byte: u8, show: Show, out: &mut Vec<u8>) {
    if byte == b'\t' {
        match show.tabs {
            true => out.extend_from_slice(b"^I"),
            false => out.push(byte),
        }
        return;
    }
    if !show.nonprinting {
        out.push(byte);
        return;
    }
    let low = match byte >= 0x80 {
        true => {
            out.extend_from_slice(b"M-");
            byte - 0x80
        }
        false => byte,
    };
    match low {
        // Below 128, a tab is shown above; above, it is `M-^I`.
        0..0x20 => out.extend_from_slice(&[b'^', low + 0x40]),
        0x7f => out.extend_from_slice(b"^?"),
        _ => out.push(low),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn show(arguments: &[&str]) -> Show {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Show::of(&run),
            other => panic!("{other:?}"),
        }
    }

    /// `pieces` fed one after the other, as from several reads or files.
    fn shown(arguments: &[&str], pieces: &[&[u8]]) -> String {
        let mut lines = Lines::new(show(arguments));
        let mut out = Vec::new();
        for piece in pieces {
            lines.feed(piece, &mut out);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn copies_plainly_without_options() {
        assert!(show(&[]).plain());
        assert!(!show(&["-s"]).plain());
        assert_eq!(shown(&[], &[b"a\tb\n\n\nc"]), "a\tb\n\n\nc");
    }

    #[test]
    fn numbers_lines_across_pieces() {
        assert_eq!(
            shown(&["-n"], &[b"a\n", b"\nb", b"c\n"]),
            "     1\ta\n     2\t\n     3\tbc\n"
        );
        assert_eq!(shown(&["-b"], &[b"a\n\nb\n"]), "     1\ta\n\n     2\tb\n");
        assert_eq!(shown(&["-nb"], &[b"\nx\n"]), "\n     1\tx\n");
    }

    #[test]
    fn squeezes_empty_lines() {
        assert_eq!(shown(&["-s"], &[b"\n\n\na\n\n", b"\n\nb\n"]), "\na\n\nb\n");
        assert_eq!(
            shown(&["-sn"], &[b"a\n\n\n\nb\n"]),
            "     1\ta\n     2\t\n     3\tb\n"
        );
    }

    #[test]
    fn shows_what_does_not_print() {
        assert_eq!(
            shown(&["-v"], &[b"\x01\t\x7f\x80\x89\xe4\n"]),
            "^A\t^?M-^@M-^IM-d\n"
        );
        assert_eq!(shown(&["-A"], &[b"a\tb\x1b\n\n"]), "a^Ib^[$\n$\n");
        assert_eq!(shown(&["-An"], &[b"x\n"]), "     1\tx$\n");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/cat.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
