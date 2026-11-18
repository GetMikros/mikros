//! What `extern` declared, by command and subcommands: the last
//! declaration of each wins.

use crate::ast::{Extern, StatementKind};
use std::collections::BTreeMap;
use std::rc::Rc;

/// The declarations, by the command's name and the subcommands below it.
#[derive(Clone, Debug, Default)]
pub struct Declarations {
    declared: BTreeMap<(String, Vec<String>), Rc<Extern>>,
}

impl Declarations {
    /// The declarations of the `extern` statements of `text`, which must
    /// parse.
    pub fn parse(text: &str) -> Declarations {
        let mut declarations = Declarations::default();
        let block = crate::parse::parse(text).expect("declarations that parse");
        for statement in block.statements {
            if let StatementKind::Extern(e) = statement.kind {
                declarations.declare(e);
            }
        }
        declarations
    }

    /// Keeps `declared`, in place of what was declared before for its
    /// command or subcommand.
    pub fn declare(&mut self, declared: Extern) {
        let key = (declared.command.clone(), declared.subcommands.clone());
        self.declared.insert(key, Rc::new(declared));
    }

    /// Whether something is declared for the command `name` at the
    /// subcommands `path` or below them.
    fn known(&self, name: &str, path: &[String]) -> bool {
        self.declared
            .keys()
            .any(|(command, below)| command == name && below.starts_with(path))
    }

    /// What the command `name`, or the subcommands `path` below it,
    /// takes, as declared last for it. A subcommand that only has
    /// subcommands of its own declared takes nothing; `None` when nothing
    /// is declared for `path` or below it.
    pub fn get(&self, name: &str, path: &[String]) -> Option<Rc<Extern>> {
        if !self.known(name, path) {
            return None;
        }
        let exact = self.declared.get(&(name.to_owned(), path.to_vec()));
        Some(exact.cloned().unwrap_or_else(|| {
            Rc::new(Extern {
                command: name.to_owned(),
                subcommands: path.to_vec(),
                ..Extern::default()
            })
        }))
    }

    /// The subcommands declared right below the subcommands `path` of the
    /// command `name`, in order.
    pub fn subcommands(&self, name: &str, path: &[String]) -> Vec<String> {
        let mut found: Vec<String> = self
            .declared
            .keys()
            .filter(|(command, below)| {
                command == name && below.len() > path.len() && below.starts_with(path)
            })
            .map(|(_, below)| below[path.len()].clone())
            .collect();
        found.dedup();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn forms(declared: &Extern) -> Vec<String> {
        declared.options.iter().flat_map(|o| o.forms()).collect()
    }

    #[test]
    fn keeps_the_last_declaration_at_each_depth() {
        let d = Declarations::parse(
            "extern git(--verbose: bool)\nextern git(--quiet(-q): bool)\n\
             extern git remote add(name: string)\nextern git remote add(url: string)",
        );
        assert_eq!(forms(&d.get("git", &[]).unwrap()), ["--quiet", "-q"]);
        let add = d.get("git", &path(&["remote", "add"])).unwrap();
        assert_eq!(add.positional[0].label, "url");
        assert!(d.get("tig", &[]).is_none());
        assert!(d.get("git", &path(&["push"])).is_none());
    }

    #[test]
    fn lists_subcommands_below_each_depth() {
        let d = Declarations::parse(
            "extern git push()\nextern git remote add()\nextern git remote remove()",
        );
        assert_eq!(d.subcommands("git", &[]), ["push", "remote"]);
        assert_eq!(d.subcommands("git", &path(&["remote"])), ["add", "remove"]);
        assert!(d.subcommands("git", &path(&["push"])).is_empty());
        // A command or subcommand declared only through those below it
        // takes nothing of its own.
        let remote = d.get("git", &path(&["remote"])).unwrap();
        assert!(remote.options.is_empty() && remote.positional.is_empty());
        assert!(d.get("git", &[]).unwrap().options.is_empty());
    }
}
