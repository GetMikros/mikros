//! What sync takes (MIKSPEC-0098.R38).

use mikros_cli::{Kind, Operand, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "the files and folders to sync, or every writable entry",
    )],
    ..Spec::new(
        "sync",
        "Has the file systems write the files and folders named to their\n\
         storage, or, without paths, the root of every entry of your namespace\n\
         that you may write.",
    )
};

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    #[test]
    fn takes_paths_and_no_options() {
        let parse = |a: &[&str]| COMMAND.parse(a.iter().map(|a| a.to_string()));
        assert!(matches!(parse(&[]), Parsed::Run(r) if r.operands.is_empty()));
        assert!(matches!(parse(&["a", "-"]), Parsed::Run(r) if r.operands == ["a", "-"]));
        assert!(matches!(parse(&["-f"]), Parsed::Invalid(_)));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/sync.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
