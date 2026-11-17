//! What the install service knows about installed packages beyond their
//! files: the package whose component one is (MIKSPEC-0101.R13), and
//! whether it was installed only because others need it
//! (MIKSPEC-0101.R29). They are kept as text, a line
//! `<scope> <id> [owner=<id>] [owner-in=<scope>] [dependency]` for each
//! package, `<scope>` being `global` or `user:<user ID>`, and `owner-in`
//! the owner's scope when it is not the component's own
//! (MIKSPEC-0101.R59).

use std::collections::BTreeMap;

/// Whom a record's package is installed for: every user, or one user, by
/// user ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    Global,
    User(u32),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The package whose component this one is.
    pub owner: Option<String>,
    /// The owner's scope when it is not this package's.
    pub owner_in: Option<Owner>,
    pub dependency: bool,
}

/// The records of installed packages, by scope and ID.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Records(pub BTreeMap<(Owner, String), Record>);

/// The scope that `word` names, `global` or `user:<user ID>`.
fn scope_of(word: &str) -> Option<Owner> {
    match word {
        "global" => Some(Owner::Global),
        user => user
            .strip_prefix("user:")
            .and_then(|u| u.parse().ok())
            .map(Owner::User),
    }
}

/// How `scope` is written.
fn scope_text(scope: Owner) -> String {
    match scope {
        Owner::Global => "global".into(),
        Owner::User(user) => format!("user:{user}"),
    }
}

impl Records {
    /// The records in `text`, leaving out lines that are not records.
    pub fn parse(text: &str) -> Records {
        let mut records = BTreeMap::new();
        for line in text.lines() {
            let mut words = line.split(' ');
            let Some(scope) = words.next().and_then(scope_of) else {
                continue;
            };
            let Some(id) = words.next().filter(|id| mikros_mika::id::is_valid(id)) else {
                continue;
            };
            let mut record = Record::default();
            for word in words {
                match word {
                    "dependency" => record.dependency = true,
                    _ => {
                        if let Some(owner) = word.strip_prefix("owner=")
                            && mikros_mika::id::is_valid(owner)
                        {
                            record.owner = Some(owner.into());
                        }
                        if let Some(text) = word.strip_prefix("owner-in=") {
                            record.owner_in = scope_of(text).filter(|s| *s != scope);
                        }
                    }
                }
            }
            records.insert((scope, id.into()), record);
        }
        Records(records)
    }

    pub fn encode(&self) -> String {
        let mut text = String::new();
        for ((scope, id), record) in &self.0 {
            text.push_str(&scope_text(*scope));
            text.push(' ');
            text.push_str(id);
            if let Some(owner) = &record.owner {
                text.push_str(&format!(" owner={owner}"));
                if let Some(owner_in) = record.owner_in.filter(|s| s != scope) {
                    text.push_str(&format!(" owner-in={}", scope_text(owner_in)));
                }
            }
            if record.dependency {
                text.push_str(" dependency");
            }
            text.push('\n');
        }
        text
    }

    pub fn get(&self, scope: Owner, id: &str) -> Record {
        self.0.get(&(scope, id.into())).cloned().unwrap_or_default()
    }

    /// Keeps `record` for `id` in `scope`, or nothing when it says
    /// nothing.
    pub fn set(&mut self, scope: Owner, id: &str, record: Record) {
        match record == Record::default() {
            true => self.0.remove(&(scope, id.into())),
            false => self.0.insert((scope, id.into()), record),
        };
    }

    /// The components of the package `owner` installed in `scope`, in
    /// either scope, each with its own scope.
    pub fn components(&self, scope: Owner, owner: &str) -> Vec<(Owner, String)> {
        self.0
            .iter()
            .filter(|((s, _), r)| {
                r.owner.as_deref() == Some(owner) && r.owner_in.unwrap_or(*s) == scope
            })
            .map(|((s, id), _)| (*s, id.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_skips_what_is_not_a_record() {
        let mut records = Records::default();
        let plugin = Record {
            owner: Some("org.example.editor".into()),
            ..Record::default()
        };
        records.set(Owner::Global, "org.example.editor.spell", plugin.clone());
        let lib = Record {
            dependency: true,
            ..Record::default()
        };
        records.set(Owner::User(1000), "org.example.lib", lib.clone());
        records.set(Owner::Global, "org.example.plain", Record::default());
        let text = records.encode();
        assert_eq!(
            text,
            "global org.example.editor.spell owner=org.example.editor\n\
             user:1000 org.example.lib dependency\n"
        );
        assert_eq!(Records::parse(&text), records);
        assert_eq!(records.get(Owner::User(1000), "org.example.lib"), lib);
        assert_eq!(
            records.get(Owner::Global, "org.example.lib"),
            Record::default()
        );
        assert_eq!(
            records.components(Owner::Global, "org.example.editor"),
            [(Owner::Global, "org.example.editor.spell".into())]
        );
        let odd = "nobody org.example.x\nuser:x org.example.x\nglobal Bad\n\nglobal org.example.y owner=Bad\n";
        let parsed = Records::parse(odd);
        assert_eq!(parsed.0.len(), 1);
        assert_eq!(
            parsed.get(Owner::Global, "org.example.y"),
            Record::default()
        );
    }

    #[test]
    fn keeps_the_scope_of_an_owner_in_another_scope() {
        let mut records = Records::default();
        let driver = Record {
            owner: Some("org.example.app".into()),
            owner_in: Some(Owner::User(1000)),
            ..Record::default()
        };
        records.set(Owner::Global, "org.example.app.driver", driver.clone());
        let same = Record {
            owner: Some("org.example.app".into()),
            owner_in: Some(Owner::User(1000)),
            ..Record::default()
        };
        records.set(Owner::User(1000), "org.example.app.plugin", same);
        let text = records.encode();
        assert_eq!(
            text,
            "global org.example.app.driver owner=org.example.app owner-in=user:1000\n\
             user:1000 org.example.app.plugin owner=org.example.app\n"
        );
        let parsed = Records::parse(&text);
        assert_eq!(parsed.get(Owner::Global, "org.example.app.driver"), driver);
        assert_eq!(
            parsed.components(Owner::User(1000), "org.example.app"),
            [
                (Owner::Global, "org.example.app.driver".into()),
                (Owner::User(1000), "org.example.app.plugin".into()),
            ]
        );
        assert!(
            parsed
                .components(Owner::Global, "org.example.app")
                .is_empty()
        );
    }
}
