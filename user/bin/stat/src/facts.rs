//! What stat takes, and how it writes the facts of a path: labelled
//! lines, the directives of `-c`, or JSON (MIKSPEC-0098.R35).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string, time};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "path",
        Kind::Path,
        "the files and folders to show",
    )],
    options: &[
        Opt::both("dereference", 'L', "follow links"),
        Opt::both(
            "format",
            'c',
            "write only <format>, with %n, %s, %F, %i, %h, %U, %a, %y and %Y",
        )
        .takes("format", Kind::Text),
        Opt::long("json", "write the facts as a JSON array of objects"),
    ],
    ..Spec::new(
        "stat",
        "Shows the type, sizes, inode, links, access, owner, stored mode, times\n\
         and access control lists of each path.",
    )
};

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    File,
    Folder,
    Link,
    Socket,
}

impl Type {
    pub fn word(self) -> &'static str {
        match self {
            Type::File => "file",
            Type::Folder => "folder",
            Type::Link => "link",
            Type::Socket => "socket",
        }
    }
}

/// Whom an entry of an access control list is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Owner,
    /// A user, by name, or by number when it has none.
    User(String),
    /// An app, by app ID, or by number when it has none.
    App(String),
    Group,
    Mask,
    Other,
    /// A tag that this version does not know, by number.
    Unknown(u32),
}

/// An entry of an access control list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AclLine {
    pub tag: Tag,
    /// The rights, as `rwx`.
    pub rights: String,
}

impl AclLine {
    /// `owner`, `user:<name>`, `app:<app ID>`, `group`, `mask` or
    /// `other`, then `:` and the rights.
    pub fn text(&self) -> String {
        let whom = match &self.tag {
            Tag::Owner => "owner".to_string(),
            Tag::User(name) => format!("user:{name}"),
            Tag::App(id) => format!("app:{id}"),
            Tag::Group => "group".to_string(),
            Tag::Mask => "mask".to_string(),
            Tag::Other => "other".to_string(),
            Tag::Unknown(raw) => format!("unknown:{raw}"),
        };
        format!("{whom}:{}", self.rights)
    }
}

/// The facts of one path.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    /// The path as given.
    pub name: String,
    /// A link's target.
    pub target: Option<String>,
    pub kind: Type,
    pub size: u64,
    /// The bytes that its storage takes.
    pub space: u64,
    pub inode: u64,
    pub links: u64,
    /// What the caller may do, as `rwx`, when it is known.
    pub access: Option<String>,
    /// The owner's name, or number.
    pub owner: String,
    /// The permission bits of the stored mode.
    pub mode: u32,
    /// Seconds since 1970, the creation time when it is known.
    pub created: Option<i64>,
    pub modified: i64,
    pub accessed: i64,
    pub changed: i64,
    /// The access control list, when it could be read.
    pub acl: Option<Vec<AclLine>>,
    /// A folder's default list, when it could be read; never for other
    /// types.
    pub default_acl: Option<Vec<AclLine>>,
}

/// The width of the labels of the text form.
const LABEL: usize = 8;

fn line(text: &mut String, label: &str, value: &str) {
    text.push_str(&format!("{label:>LABEL$}: {value}\n"));
}

/// The lines of an access control list under `label`, the first beside
/// it and the others below, or `-` when it is unknown or empty.
fn acl_lines(text: &mut String, label: &str, acl: Option<&[AclLine]>, paint: Paint) {
    match acl {
        Some(entries) if !entries.is_empty() => {
            for (i, entry) in entries.iter().enumerate() {
                let label = if i == 0 { label } else { "" };
                let shown = entry.text();
                match label.is_empty() {
                    true => text.push_str(&format!("{:LABEL$}  {shown}\n", "")),
                    false => line(text, label, &shown),
                }
            }
        }
        _ => line(text, label, &paint.paint(Role::Secondary, "-")),
    }
}

/// The facts as labelled lines, one fact on each.
pub fn text(facts: &Facts, paint: Paint) -> String {
    let mut text = String::new();
    let number = |n: u64| paint.paint(Role::Number, &n.to_string());
    let when = |t: i64| paint.paint(Role::Secondary, &time::local(t));
    let mut name = paint.entry(&facts.name);
    if let Some(target) = &facts.target {
        let kind = paint.target_type(&facts.name, target);
        name.push_str(" -> ");
        name.push_str(&paint.paint(Role::Path(kind), target));
    }
    line(&mut text, "Name", &name);
    line(&mut text, "Type", facts.kind.word());
    line(&mut text, "Size", &number(facts.size));
    line(&mut text, "Space", &number(facts.space));
    line(&mut text, "Inode", &number(facts.inode));
    line(&mut text, "Links", &number(facts.links));
    let access = match &facts.access {
        Some(access) => access.clone(),
        None => paint.paint(Role::Secondary, "-"),
    };
    line(&mut text, "Access", &access);
    line(&mut text, "Owner", &facts.owner);
    let mode = format!("{:04o} (stored, for compatibility only)", facts.mode);
    line(&mut text, "Mode", &mode);
    let created = match facts.created {
        Some(t) => when(t),
        None => paint.paint(Role::Secondary, "-"),
    };
    line(&mut text, "Created", &created);
    line(&mut text, "Modified", &when(facts.modified));
    line(&mut text, "Accessed", &when(facts.accessed));
    line(&mut text, "Changed", &when(facts.changed));
    acl_lines(&mut text, "ACL", facts.acl.as_deref(), paint);
    if facts.kind == Type::Folder {
        acl_lines(&mut text, "Default", facts.default_acl.as_deref(), paint);
    }
    text
}

/// `format` with the directives of `-c` replaced by the facts, and `%%`
/// by `%`; other directives stay as they are.
pub fn format(format: &str, facts: &Facts) -> String {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(directive) = chars.next() else {
            out.push('%');
            break;
        };
        match directive {
            'n' => out.push_str(&facts.name),
            's' => out.push_str(&facts.size.to_string()),
            'F' => out.push_str(facts.kind.word()),
            'i' => out.push_str(&facts.inode.to_string()),
            'h' => out.push_str(&facts.links.to_string()),
            'U' => out.push_str(&facts.owner),
            'a' => out.push_str(facts.access.as_deref().unwrap_or("-")),
            'y' => out.push_str(&time::local(facts.modified)),
            'Y' => out.push_str(&facts.modified.to_string()),
            '%' => out.push('%'),
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out.push('\n');
    out
}

fn json_acl(acl: Option<&[AclLine]>) -> String {
    match acl {
        Some(entries) => {
            let lines: Vec<String> = entries.iter().map(|e| json_string(&e.text())).collect();
            format!("[{}]", lines.join(","))
        }
        None => "null".into(),
    }
}

fn json_or_null(text: Option<&str>) -> String {
    text.map_or_else(|| "null".into(), json_string)
}

/// The facts of each path as a JSON array of objects, on one line.
pub fn json(all: &[Facts]) -> String {
    let objects: Vec<String> = all
        .iter()
        .map(|f| {
            let created = f.created.map(time::utc);
            format!(
                "{{\"name\":{},\"target\":{},\"type\":\"{}\",\"size\":{},\"space\":{},\
                 \"inode\":{},\"links\":{},\"access\":{},\"owner\":{},\"stored_mode\":\"{:04o}\",\
                 \"created\":{},\"modified\":{},\"accessed\":{},\"changed\":{},\"acl\":{},\
                 \"default_acl\":{}}}",
                json_string(&f.name),
                json_or_null(f.target.as_deref()),
                f.kind.word(),
                f.size,
                f.space,
                f.inode,
                f.links,
                json_or_null(f.access.as_deref()),
                json_string(&f.owner),
                f.mode,
                json_or_null(created.as_deref()),
                json_string(&time::utc(f.modified)),
                json_string(&time::utc(f.accessed)),
                json_string(&time::utc(f.changed)),
                json_acl(f.acl.as_deref()),
                json_acl(f.default_acl.as_deref()),
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

/// How a run writes: labelled lines, a format, or JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Form {
    Text,
    Format(String),
    Json,
}

impl Form {
    pub fn of(run: &Run) -> Form {
        if run.has("--json") {
            Form::Json
        } else if let Some(format) = run.value("-c") {
            Form::Format(format.into())
        } else {
            Form::Text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn run(arguments: &[&str]) -> Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    fn sample() -> Facts {
        Facts {
            name: "notes".into(),
            target: None,
            kind: Type::File,
            size: 12,
            space: 4096,
            inode: 77,
            links: 1,
            access: Some("rw-".into()),
            owner: "neo".into(),
            mode: 0o644,
            created: None,
            modified: 300,
            accessed: 300,
            changed: 300,
            acl: Some(vec![
                AclLine {
                    tag: Tag::Owner,
                    rights: "rw-".into(),
                },
                AclLine {
                    tag: Tag::User("sip".into()),
                    rights: "r--".into(),
                },
                AclLine {
                    tag: Tag::App("7".into()),
                    rights: "r--".into(),
                },
                AclLine {
                    tag: Tag::Other,
                    rights: "---".into(),
                },
            ]),
            default_acl: None,
        }
    }

    #[test]
    fn reads_the_form() {
        assert_eq!(Form::of(&run(&["a"])), Form::Text);
        assert_eq!(
            Form::of(&run(&["-c", "%n", "a"])),
            Form::Format("%n".into())
        );
        assert_eq!(Form::of(&run(&["-c%s", "a"])), Form::Format("%s".into()));
        assert_eq!(Form::of(&run(&["--json", "-c", "%n", "a"])), Form::Json);
        assert!(run(&["-L", "a"]).has("--dereference"));
        assert!(matches!(
            COMMAND.parse(Vec::<String>::new()),
            Parsed::Invalid(_)
        ));
    }

    #[test]
    fn writes_labelled_lines() {
        let text = text(&sample(), Paint::never());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "    Name: notes");
        assert_eq!(lines[1], "    Type: file");
        assert_eq!(lines[3], "   Space: 4096");
        assert_eq!(lines[6], "  Access: rw-");
        assert_eq!(lines[7], "   Owner: neo");
        assert_eq!(lines[8], "    Mode: 0644 (stored, for compatibility only)");
        assert_eq!(lines[9], " Created: -");
        assert!(lines[10].starts_with("Modified: "));
        assert_eq!(lines[13], "     ACL: owner:rw-");
        assert_eq!(lines[14], "          user:sip:r--");
        assert_eq!(lines[15], "          app:7:r--");
        assert_eq!(lines[16], "          other:---");
        assert_eq!(lines.len(), 17);
    }

    #[test]
    fn shows_links_and_default_lists_of_folders() {
        let mut link = sample();
        link.kind = Type::Link;
        link.target = Some("elsewhere".into());
        link.acl = None;
        let text = text(&link, Paint::never());
        assert!(text.starts_with("    Name: notes -> elsewhere\n"));
        assert!(text.ends_with("     ACL: -\n"));
        let mut folder = sample();
        folder.kind = Type::Folder;
        folder.acl = Some(vec![]);
        folder.default_acl = Some(vec![AclLine {
            tag: Tag::Mask,
            rights: "r-x".into(),
        }]);
        let text = super::text(&folder, Paint::never());
        assert!(
            text.ends_with("     ACL: -\n Default: mask:r-x\n"),
            "{text}"
        );
    }

    #[test]
    fn writes_formats() {
        let facts = sample();
        assert_eq!(
            format("%n %s %F %i %h %U %a %Y", &facts),
            "notes 12 file 77 1 neo rw- 300\n"
        );
        assert_eq!(format("100%% %q %", &facts), "100% %q %\n");
        assert_eq!(format("%y", &facts).len(), 17);
    }

    #[test]
    fn writes_json() {
        let json = json(&[sample()]);
        assert!(json.starts_with(
            "[{\"name\":\"notes\",\"target\":null,\"type\":\"file\",\"size\":12,\"space\":4096,\
             \"inode\":77,\"links\":1,\"access\":\"rw-\",\"owner\":\"neo\",\"stored_mode\":\"0644\",\
             \"created\":null,\"modified\":\"1970-01-01T00:05:00Z\","
        ));
        assert!(json.ends_with(
            "\"acl\":[\"owner:rw-\",\"user:sip:r--\",\"app:7:r--\",\"other:---\"],\
             \"default_acl\":null}]\n"
        ));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/stat.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
