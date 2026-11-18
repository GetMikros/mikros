//! What sudo takes: options before the command, and everything from the
//! command on as the command's (MIKSPEC-0098.R31); and the reason it
//! gives the session manager.

use mikros_cli::{Kind, Operand, Spec};

/// The longest reason that `Elevate` takes, in bytes.
const REASON_MAX: usize = 256;

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one(
            "command",
            Kind::Command,
            "the command to run as an authorized administrator",
        ),
        Operand::any("arguments", Kind::Text, "the command's arguments"),
    ],
    stops: true,
    ..Spec::new(
        "sudo",
        "Runs command as an authorized administrator: once an administrator's\n\
         password is given, the command and every process it starts may perform\n\
         administrator actions without being asked again.",
    )
};

/// The command line as the reason for `Elevate`, cut to what it takes at
/// a character boundary.
pub fn reason(command: &[String]) -> String {
    let mut reason = command.join(" ");
    let mut end = reason.len().min(REASON_MAX);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.truncate(end);
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn read(arguments: &[&str]) -> Parsed {
        COMMAND.parse(arguments.iter().map(|a| a.to_string()))
    }

    fn command(arguments: &[&str]) -> Vec<String> {
        match read(arguments) {
            Parsed::Run(run) => run.operands,
            other => panic!("{arguments:?} gave {other:?}"),
        }
    }

    #[test]
    fn reads_the_command_and_its_arguments() {
        assert_eq!(command(&["power", "off"]), ["power", "off"]);
        assert_eq!(command(&["ls", "--help"]), ["ls", "--help"]);
        assert_eq!(command(&["--", "-weird"]), ["-weird"]);
        assert_eq!(command(&["ls", "--", "-l"]), ["ls", "--", "-l"]);
        assert_eq!(read(&["--help"]), Parsed::Help(vec![]));
    }

    #[test]
    fn refuses_a_missing_command_and_unknown_options() {
        let invalid = |reason: &str| Parsed::Invalid(reason.into());
        assert_eq!(read(&[]), invalid("missing operand <command>"));
        assert_eq!(read(&["--"]), invalid("missing operand <command>"));
        assert_eq!(read(&["-u", "root"]), invalid("unknown option -u"));
    }

    #[test]
    fn gives_the_command_line_as_the_reason() {
        let command = ["power".to_string(), "off".to_string()];
        assert_eq!(reason(&command), "power off");
        let long = ["ä".repeat(200)];
        let cut = reason(&long);
        assert!(cut.len() <= REASON_MAX && cut.chars().all(|c| c == 'ä'));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/sudo.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
