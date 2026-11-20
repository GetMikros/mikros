//! desktop, which starts the desktop from the text console: it calls
//! `StartDesktop` on the session manager's `Desktop`, exits with 0 once
//! the desktop has started, and with 1 and the reason when it has not
//! (MIKSPEC-0098.R72). `program`, left out of the host's build, makes the
//! call.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;

use mikros_cli::{Kind, Spec};

pub static COMMAND: Spec = Spec::new("desktop", "Starts the desktop from the text console.");

/// Names of system things for completion, of which desktop takes none.
pub fn nothing(_: Kind, _: &str) -> Vec<String> {
    Vec::new()
}

/// What desktop says when `StartDesktop` failed with the status named
/// `status`, as MIDL writes it in Rust.
pub fn failure(status: &str) -> String {
    match status {
        "NotSupported" => "no desktop is installed".into(),
        "AlreadyExists" => "a desktop is already running".into(),
        "AccessDenied" => "not on a text console".into(),
        other => format!("StartDesktop failed: {other}"),
    }
}

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    #[test]
    fn takes_no_operands() {
        assert!(matches!(COMMAND.parse(Vec::new()), Parsed::Run(_)));
        assert_eq!(
            COMMAND.parse(vec!["x".to_string()]),
            Parsed::Invalid("unexpected argument x".into())
        );
    }

    #[test]
    fn names_the_failures() {
        assert_eq!(failure("NotSupported"), "no desktop is installed");
        assert_eq!(failure("AlreadyExists"), "a desktop is already running");
        assert_eq!(failure("AccessDenied"), "not on a text console");
        assert_eq!(failure("NoResources"), "StartDesktop failed: NoResources");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/desktop.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
