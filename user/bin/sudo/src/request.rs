//! What sudo is asked to run, from its arguments, and the reason it
//! gives the session manager.

/// The longest reason that `Elevate` takes, in bytes.
const REASON_MAX: usize = 256;

/// What sudo does.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// Runs the command, the first of these, with the rest as its
    /// arguments.
    Run(Vec<String>),
    Help,
    /// The arguments are invalid, for the reason given.
    Invalid(String),
}

pub const HELP: &str = "Usage: sudo command [arguments]\n\n\
Runs command as an authorized administrator: once an administrator's\n\
password is given, the command and every process it starts may perform\n\
administrator actions without being asked again.\n\n\
Options:\n  --help  show this help and exit\n";

/// What `arguments`, those after the command's name, ask. Options come
/// before the command; everything from the command on is the command's.
pub fn read(arguments: Vec<String>) -> Request {
    match arguments.first().map(String::as_str) {
        None => Request::Invalid("the command is missing".into()),
        Some("--help") => Request::Help,
        Some("--") if arguments.len() > 1 => Request::Run(arguments[1..].to_vec()),
        Some("--") => Request::Invalid("the command is missing".into()),
        Some(option) if option.starts_with('-') => {
            Request::Invalid(format!("unknown option {option}"))
        }
        Some(_) => Request::Run(arguments),
    }
}

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

    fn read_all(arguments: &[&str]) -> Request {
        read(arguments.iter().map(|a| a.to_string()).collect())
    }

    #[test]
    fn reads_the_command_and_its_arguments() {
        let run = |command: &[&str]| Request::Run(command.iter().map(|a| a.to_string()).collect());
        assert_eq!(read_all(&["power", "off"]), run(&["power", "off"]));
        assert_eq!(read_all(&["ls", "--help"]), run(&["ls", "--help"]));
        assert_eq!(read_all(&["--", "-weird"]), run(&["-weird"]));
        assert_eq!(read_all(&["--help"]), Request::Help);
    }

    #[test]
    fn refuses_a_missing_command_and_unknown_options() {
        let invalid = |reason: &str| Request::Invalid(reason.into());
        assert_eq!(read_all(&[]), invalid("the command is missing"));
        assert_eq!(read_all(&["--"]), invalid("the command is missing"));
        assert_eq!(read_all(&["-u", "root"]), invalid("unknown option -u"));
    }

    #[test]
    fn gives_the_command_line_as_the_reason() {
        let command = ["power".to_string(), "off".to_string()];
        assert_eq!(reason(&command), "power off");
        let long = ["ä".repeat(200)];
        let cut = reason(&long);
        assert!(cut.len() <= REASON_MAX && cut.chars().all(|c| c == 'ä'));
    }
}
