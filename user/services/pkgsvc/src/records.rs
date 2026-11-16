//! What the install service knows about installed packages beyond their
//! files: the package whose component one is (MIKSPEC-0101.R13), and
//! whether it was installed only because others need it
//! (MIKSPEC-0101.R29). They are kept as text, a line
//! `<scope> <id> [owner=<id>] [dependency]` for each package, `<scope>`
//! being `global` or `user:<user ID>`.

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
    pub dependency: bool,
}

/// The records of installed packages, by scope and ID.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Records(pub BTreeMap<(Owner, String), Record>);

impl Records {
    /// The records in `text`, leaving out lines that are not records.
    pub fn parse(text: &str) -> Records {
        let mut records = BTreeMap::new();
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
            match scope {
                Owner::Global => text.push_str("global"),
                Owner::User(user) => text.push_str(&format!("user:{user}")),
            }
            text.push(' ');
            text.push_str(id);
            if let Some(owner) = &record.owner {
                text.push_str(&format!(" owner={owner}"));
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

    /// The components of `owner` in `scope`.
    pub fn components(&self, scope: Owner, owner: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|((s, _), r)| *s == scope && r.owner.as_deref() == Some(owner))
            .map(|((_, id), _)| id.clone())
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
            dependency: false,
        };
        records.set(Owner::Global, "org.example.editor.spell", plugin.clone());
        let lib = Record {
            owner: None,
            dependency: true,
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
            ["org.example.editor.spell"]
        );
        let odd = "nobody org.example.x\nuser:x org.example.x\nglobal Bad\n\nglobal org.example.y owner=Bad\n";
        let parsed = Records::parse(odd);
        assert_eq!(parsed.0.len(), 1);
        assert_eq!(
            parsed.get(Owner::Global, "org.example.y"),
            Record::default()
        );
    }
}
