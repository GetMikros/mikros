//! What power is asked to do, from its arguments.

use mikros_cli::{Arguments, Command};

pub const COMMAND: Command = Command {
    name: "power",
    operands: "off|restart",
    most: 1,
    about: "Powers the machine off, or restarts it, once an administrator\n\
            allowed it.",
    options: &[],
    values: &[],
};

/// What power does.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Off,
    Restart,
    Help,
    /// The arguments are invalid, for the reason given.
    Invalid(String),
}

/// What `arguments`, those after the command's name, ask.
pub fn read(arguments: impl IntoIterator<Item = String>) -> Action {
    match COMMAND.parse(arguments) {
        Arguments::Help => Action::Help,
        Arguments::Invalid(reason) => Action::Invalid(reason),
        Arguments::Run { operands, .. } => match operands.first().map(String::as_str) {
            Some("off") => Action::Off,
            Some("restart") => Action::Restart,
            Some(other) => Action::Invalid(format!("unknown action {other}")),
            None => Action::Invalid("off or restart is missing".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(arguments: &[&str]) -> Action {
        read(arguments.iter().map(|a| a.to_string()))
    }

    #[test]
    fn reads_the_actions() {
        assert_eq!(read_all(&["off"]), Action::Off);
        assert_eq!(read_all(&["restart"]), Action::Restart);
        assert_eq!(read_all(&["off", "--help"]), Action::Help);
    }

    #[test]
    fn refuses_other_arguments() {
        let invalid = |reason: &str| Action::Invalid(reason.into());
        assert_eq!(read_all(&[]), invalid("off or restart is missing"));
        assert_eq!(read_all(&["sideways"]), invalid("unknown action sideways"));
        assert_eq!(
            read_all(&["off", "now"]),
            invalid("unexpected argument now")
        );
        assert_eq!(read_all(&["--now"]), invalid("unknown option --now"));
    }
}
