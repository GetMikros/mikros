//! Colours by role (MIKSPEC-0098.R31): whether a stream is coloured, from
//! `--color`, whether it is a terminal and `NO_COLOR`, and the palette,
//! after the defaults of msh's syntax colours (MIKSPEC-0084.R62).

use crate::spec::Run;
use std::io::IsTerminal;

/// What a piece of output is, which gives its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Names of files and folders.
    Path,
    /// Sizes, counts and IDs.
    Number,
    /// Free text, such as full names and messages.
    String,
    /// Failures, denied and failed states, the severity `ERROR`.
    Error,
    /// The severity `WARNING`, and states such as restarting.
    Warning,
    /// Running and allowed states.
    Ok,
    /// Headers, `-`, times and debug records.
    Secondary,
}

impl Role {
    /// The SGR parameters of its colour.
    fn sgr(self) -> &'static str {
        match self {
            Role::Path => "38;2;27;213;177",
            Role::Number => "38;2;21;193;160",
            Role::String => "38;2;255;134;26",
            Role::Error => "38;2;231;76;60",
            Role::Warning => "33",
            Role::Ok => "38;2;17;209;22",
            Role::Secondary => "90",
        }
    }
}

/// Whether one stream is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint(bool);

impl Paint {
    /// Colouring by `--color`'s `when`, for a stream that is a terminal
    /// when `terminal`, with `no_color` the value of `NO_COLOR`.
    pub fn new(when: Option<&str>, terminal: bool, no_color: Option<&str>) -> Paint {
        Paint(match when {
            Some("always") => true,
            Some("never") => false,
            _ => terminal && no_color.is_none_or(str::is_empty),
        })
    }

    /// Colouring of standard output for `run`.
    pub fn stdout(run: &Run) -> Paint {
        Paint::from_env(run.value("--color"), std::io::stdout().is_terminal())
    }

    /// Colouring of standard error for `run`.
    pub fn stderr(run: &Run) -> Paint {
        Paint::from_env(run.value("--color"), std::io::stderr().is_terminal())
    }

    fn from_env(when: Option<&str>, terminal: bool) -> Paint {
        let no_color = std::env::var("NO_COLOR").ok();
        Paint::new(when, terminal, no_color.as_deref())
    }

    /// No colour, for `--json` and for tests.
    pub const fn never() -> Paint {
        Paint(false)
    }

    pub fn on(self) -> bool {
        self.0
    }

    /// `text` in the colour of `role`, when the stream is coloured.
    pub fn paint(self, role: Role, text: &str) -> String {
        match self.0 && !text.is_empty() {
            true => format!("\x1b[{}m{text}\x1b[0m", role.sgr()),
            false => text.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_by_option_terminal_and_no_color() {
        assert!(Paint::new(None, true, None).on());
        assert!(!Paint::new(None, false, None).on());
        assert!(!Paint::new(Some("auto"), true, Some("1")).on());
        assert!(Paint::new(Some("auto"), true, Some("")).on());
        assert!(Paint::new(Some("always"), false, Some("1")).on());
        assert!(!Paint::new(Some("never"), true, None).on());
    }

    #[test]
    fn paints_by_role() {
        let on = Paint::new(Some("always"), false, None);
        assert_eq!(on.paint(Role::Path, "a"), "\x1b[38;2;27;213;177ma\x1b[0m");
        assert_eq!(on.paint(Role::Secondary, "-"), "\x1b[90m-\x1b[0m");
        assert_eq!(on.paint(Role::Warning, ""), "");
        assert_eq!(Paint::never().paint(Role::Error, "x"), "x");
    }
}
