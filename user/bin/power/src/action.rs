//! What power takes: the subcommands `off` and `restart`
//! (MIKSPEC-0098.R29).

use mikros_cli::{Run, Spec};

pub static COMMAND: Spec = Spec {
    subcommands: &[
        Spec::new("off", "Powers the machine off."),
        Spec::new("restart", "Restarts the machine."),
    ],
    ..Spec::new(
        "power",
        "Powers the machine off, or restarts it, once an administrator\n\
         allowed it.",
    )
};

/// What power does.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Off,
    Restart,
}

/// What `run` asks, or why its arguments are invalid.
pub fn read(run: &Run) -> Result<Action, String> {
    match run.subcommand() {
        Some("off") => Ok(Action::Off),
        Some("restart") => Ok(Action::Restart),
        _ => Err("off or restart is missing".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn read_all(arguments: &[&str]) -> Result<Action, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => read(&run),
            Parsed::Invalid(reason) => Err(reason),
            Parsed::Help(path) => Err(format!("help for {path:?}")),
        }
    }

    #[test]
    fn reads_the_actions() {
        assert_eq!(read_all(&["off"]), Ok(Action::Off));
        assert_eq!(read_all(&["restart"]), Ok(Action::Restart));
        assert_eq!(
            read_all(&["off", "--help"]),
            Err("help for [\"off\"]".into())
        );
    }

    #[test]
    fn refuses_other_arguments() {
        let invalid = |reason: &str| Err(reason.into());
        assert_eq!(read_all(&[]), invalid("off or restart is missing"));
        assert_eq!(
            read_all(&["sideways"]),
            invalid("unexpected argument sideways")
        );
        assert_eq!(
            read_all(&["off", "now"]),
            invalid("unexpected argument now")
        );
        assert_eq!(read_all(&["--now"]), invalid("unknown option --now"));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/power.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
