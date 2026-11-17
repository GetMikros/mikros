//! The commands of installed packages (MIKSPEC-0101.R44): which package of
//! a scope has each command name, which of them owns it when several have
//! it, as installs chose (MIKSPEC-0101.R43), and which commands a user
//! sees, a local one hiding a global one of the same name. The owners are
//! kept as text, a line `<scope> <name> <id>` for each command that more
//! than one package had when one of them was installed, `<scope>` being
//! `global` or `user:<user ID>`.

use crate::records::Owner;
use std::collections::BTreeMap;

/// A command of an installed package: the package's ID, and the path of
/// its file from the root of the root file system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub id: String,
    pub file: String,
}

/// The commands that the packages of a scope have, by name, each with
/// every package that has it.
pub type Provided = BTreeMap<String, Vec<Command>>;

/// The owners of commands, by scope and name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Owners(pub BTreeMap<(Owner, String), String>);

impl Owners {
    /// The owners in `text`, leaving out lines that are not owners.
    pub fn parse(text: &str) -> Owners {
        let mut owners = BTreeMap::new();
        for line in text.lines() {
            let mut words = line.split(' ');
            let scope = match words.next() {
                Some("global") => Owner::Global,
                Some(user) => match user.strip_prefix("user:").and_then(|u| u.parse().ok()) {
                    Some(user) => Owner::User(user),
                    None => continue,
                },
                None => continue,
            };
            let (Some(name), Some(id), None) = (words.next(), words.next(), words.next()) else {
                continue;
            };
            if mikros_mika::manifest::is_command(name) && mikros_mika::id::is_valid(id) {
                owners.insert((scope, name.into()), id.into());
            }
        }
        Owners(owners)
    }

    pub fn encode(&self) -> String {
        let mut text = String::new();
        for ((scope, name), id) in &self.0 {
            match scope {
                Owner::Global => text.push_str("global"),
                Owner::User(user) => text.push_str(&format!("user:{user}")),
            }
            text.push_str(&format!(" {name} {id}\n"));
        }
        text
    }

    /// Forgets the commands that the package `id` of `scope` owns, which
    /// then go to another package that has them.
    pub fn forget(&mut self, scope: Owner, id: &str) {
        self.0.retain(|(s, _), owner| *s != scope || owner != id);
    }

    /// The package of `providers`, those of `scope` that have the command
    /// `name`, that owns it: the recorded one while it still has it, and
    /// otherwise the one with the first ID.
    pub fn owner<'a>(
        &self,
        scope: Owner,
        name: &str,
        providers: &'a [Command],
    ) -> Option<&'a Command> {
        let recorded = self.0.get(&(scope, name.into()));
        recorded
            .and_then(|id| providers.iter().find(|c| &c.id == id))
            .or_else(|| providers.iter().min_by(|a, b| a.id.cmp(&b.id)))
    }

    /// The commands of `scope`, by name, with the package that owns each.
    pub fn resolve(&self, scope: Owner, provided: &Provided) -> BTreeMap<String, Command> {
        provided
            .iter()
            .filter_map(|(name, providers)| {
                let owner = self.owner(scope, name, providers)?;
                Some((name.clone(), owner.clone()))
            })
            .collect()
    }

    /// Records who keeps each of `names`, the commands of the package `id`
    /// being installed in `scope`, that another package of `others`, the
    /// commands of the scope's other packages, has: `id` for those that
    /// `taken` names, and the package that owns it now otherwise.
    pub fn install(
        &mut self,
        scope: Owner,
        id: &str,
        names: &[String],
        taken: &[String],
        others: &Provided,
    ) {
        for name in names {
            let Some(providers) = others.get(name).filter(|p| !p.is_empty()) else {
                continue;
            };
            let keeper = match taken.contains(name) {
                true => String::from(id),
                false => match self.owner(scope, name, providers) {
                    Some(current) => current.id.clone(),
                    None => continue,
                },
            };
            self.0.insert((scope, name.clone()), keeper);
        }
    }
}

/// The commands that a user sees: `local`, theirs, and those of `global`
/// whose names no local one has.
pub fn visible(
    global: BTreeMap<String, Command>,
    local: BTreeMap<String, Command>,
) -> BTreeMap<String, Command> {
    let mut seen = global;
    seen.extend(local);
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(id: &str) -> Command {
        Command {
            id: id.into(),
            file: format!("apps/{id}/bin/x"),
        }
    }

    fn provided(entries: &[(&str, &[&str])]) -> Provided {
        entries
            .iter()
            .map(|(name, ids)| ((*name).into(), ids.iter().map(|id| command(id)).collect()))
            .collect()
    }

    #[test]
    fn round_trips_and_skips_what_is_not_an_owner() {
        let mut owners = Owners::default();
        owners
            .0
            .insert((Owner::Global, "edit".into()), "org.example.a".into());
        owners
            .0
            .insert((Owner::User(1000), "edit".into()), "org.example.b".into());
        let text = owners.encode();
        assert_eq!(
            text,
            "global edit org.example.a\nuser:1000 edit org.example.b\n"
        );
        let noisy = format!(
            "{text}nobody edit org.x\nglobal Edit org.x\nglobal edit\nglobal edit org.x more\n"
        );
        assert_eq!(Owners::parse(&noisy), owners);
    }

    #[test]
    fn keeps_a_command_with_its_package_unless_taken() {
        let mut owners = Owners::default();
        let before = provided(&[("edit", &["org.example.b"])]);
        let names = ["edit".to_string(), "view".to_string()];
        // The new package's ID sorts first, but the old one keeps it.
        owners.install(Owner::Global, "org.example.a", &names, &[], &before);
        let after = provided(&[
            ("edit", &["org.example.a", "org.example.b"]),
            ("view", &["org.example.a"]),
        ]);
        let seen = owners.resolve(Owner::Global, &after);
        assert_eq!(seen["edit"].id, "org.example.b");
        assert_eq!(seen["view"].id, "org.example.a");
        // Taken over by the next install.
        owners.install(
            Owner::Global,
            "org.example.a",
            &names,
            &names[..1],
            &provided(&[("edit", &["org.example.b"])]),
        );
        assert_eq!(
            owners.resolve(Owner::Global, &after)["edit"].id,
            "org.example.a"
        );
    }

    #[test]
    fn gives_a_removed_owners_commands_to_another_package() {
        let mut owners = Owners::default();
        owners
            .0
            .insert((Owner::Global, "edit".into()), "org.example.c".into());
        owners.forget(Owner::Global, "org.example.c");
        let left = provided(&[("edit", &["org.example.d", "org.example.b"])]);
        assert_eq!(
            owners.resolve(Owner::Global, &left)["edit"].id,
            "org.example.b"
        );
        assert!(owners.0.is_empty());
    }

    #[test]
    fn hides_a_global_command_behind_a_local_one() {
        let owners = Owners::default();
        let global = owners.resolve(
            Owner::Global,
            &provided(&[("edit", &["org.g"]), ("view", &["org.g"])]),
        );
        let local = owners.resolve(Owner::User(1000), &provided(&[("edit", &["org.l"])]));
        let seen = visible(global, local);
        assert_eq!(seen["edit"].id, "org.l");
        assert_eq!(seen["view"].id, "org.g");
    }
}
