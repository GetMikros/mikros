//! What hostname takes, and the short form of the host name
//! (MIKSPEC-0098.R55). It takes no operand.

use mikros_cli::{Opt, Spec};

pub static COMMAND: Spec = Spec {
    options: &[Opt::both(
        "short",
        's',
        "only the part before the first dot",
    )],
    ..Spec::new(
        "hostname",
        "Writes the host name, which /system/config/hostname sets.",
    )
};

/// The host name as written: the part before the first dot with
/// `short`, and the whole name otherwise.
pub fn shown(name: &str, short: bool) -> &str {
    match short {
        true => name.split('.').next().unwrap_or(name),
        false => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    #[test]
    fn shortens_at_the_first_dot() {
        assert_eq!(shown("mikros", true), "mikros");
        assert_eq!(shown("box.example.org", true), "box");
        assert_eq!(shown("box.example.org", false), "box.example.org");
    }

    #[test]
    fn takes_no_operand() {
        let parsed = COMMAND.parse(["newname".to_string()]);
        assert_eq!(
            parsed,
            Parsed::Invalid("unexpected argument newname".into())
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/hostname.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
