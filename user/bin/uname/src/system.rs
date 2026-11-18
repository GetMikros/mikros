//! What uname takes, and the fields it writes, in the order of `-a`, or
//! an object in JSON (MIKSPEC-0098.R56).

use mikros_cli::{Opt, Run, Spec, json_string};

pub static COMMAND: Spec = Spec {
    options: &[
        Opt::both("all", 'a', "every field, in the order of the options below"),
        Opt::both("kernel-name", 's', "the system's name, Mikros"),
        Opt::both("nodename", 'n', "the host name"),
        Opt::both("kernel-release", 'r', "the booted system version"),
        Opt::both("kernel-version", 'v', "the booted system version"),
        Opt::both("machine", 'm', "the machine, x86_64"),
        Opt::both("processor", 'p', "the processor, x86_64"),
        Opt::both("operating-system", 'o', "the operating system, Mikros"),
        Opt::long(
            "json",
            "write an object with system, node, release, machine and os",
        ),
    ],
    ..Spec::new(
        "uname",
        "Writes what system this is: Mikros without options, and the fields\n\
         that the options name, in their order, with -a all of them.",
    )
};

const SYSTEM: &str = "Mikros";
const MACHINE: &str = "x86_64";
/// What stands for the version when bootsvc knows none.
pub const UNKNOWN: &str = "unknown";

/// A field of the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    System,
    Node,
    Release,
    Version,
    Machine,
    Processor,
    Os,
}

/// Each field with its option, in the order of `-a`.
const FIELDS: [(Field, &str); 7] = [
    (Field::System, "-s"),
    (Field::Node, "-n"),
    (Field::Release, "-r"),
    (Field::Version, "-v"),
    (Field::Machine, "-m"),
    (Field::Processor, "-p"),
    (Field::Os, "-o"),
];

/// The fields that `run` asks for, in order: those its options name,
/// all with `-a`, and the system's name without any.
pub fn fields(run: &Run) -> Vec<Field> {
    let all = run.has("-a");
    let named: Vec<Field> = FIELDS
        .iter()
        .filter(|(_, option)| all || run.has(option))
        .map(|(field, _)| *field)
        .collect();
    match named.is_empty() {
        true => vec![Field::System],
        false => named,
    }
}

/// What the fields show: the host name, and the booted version, `None`
/// when bootsvc knows none.
pub struct Facts {
    pub node: String,
    pub version: Option<String>,
}

/// The line of `fields`.
pub fn line(fields: &[Field], facts: &Facts) -> String {
    let version = facts.version.as_deref().unwrap_or(UNKNOWN);
    let words: Vec<&str> = fields
        .iter()
        .map(|field| match field {
            Field::System | Field::Os => SYSTEM,
            Field::Node => &facts.node,
            Field::Release | Field::Version => version,
            Field::Machine | Field::Processor => MACHINE,
        })
        .collect();
    format!("{}\n", words.join(" "))
}

/// The object of `--json`, with `release` null when bootsvc knows no
/// version.
pub fn json(facts: &Facts) -> String {
    let release = facts.version.as_deref().map_or("null".into(), json_string);
    format!(
        "{{\"system\":\"{SYSTEM}\",\"node\":{},\"release\":{release},\"machine\":\"{MACHINE}\",\"os\":\"{SYSTEM}\"}}\n",
        json_string(&facts.node)
    )
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

    fn facts(version: Option<&str>) -> Facts {
        Facts {
            node: "box".into(),
            version: version.map(String::from),
        }
    }

    #[test]
    fn writes_the_fields_in_order() {
        let known = facts(Some("2026.11.1"));
        let line_of = |arguments: &[&str]| line(&fields(&run(arguments)), &known);
        assert_eq!(line_of(&[]), "Mikros\n");
        assert_eq!(line_of(&["-s"]), "Mikros\n");
        assert_eq!(line_of(&["-mn"]), "box x86_64\n");
        assert_eq!(line_of(&["-o", "-r"]), "2026.11.1 Mikros\n");
        assert_eq!(
            line_of(&["-a"]),
            "Mikros box 2026.11.1 2026.11.1 x86_64 x86_64 Mikros\n"
        );
        assert_eq!(line(&[Field::Version], &facts(None)), "unknown\n");
    }

    #[test]
    fn writes_json() {
        assert_eq!(
            json(&facts(Some("2026.11.1"))),
            "{\"system\":\"Mikros\",\"node\":\"box\",\"release\":\"2026.11.1\",\
             \"machine\":\"x86_64\",\"os\":\"Mikros\"}\n"
        );
        assert!(json(&facts(None)).contains("\"release\":null,"));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/uname.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
