//! What a utility takes, as tables: its options, operands and
//! subcommands, from which the arguments of a run are read, and from
//! which `usage` writes `--help` and the declaration for msh's
//! completion, so that the three always agree.
//!
//! Every utility also takes `--help` and `--color`, which the tables
//! leave out.

use std::fmt::Write;

/// The kind of an operand or of an option's value, which says how msh
/// completes and colours it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Int,
    /// A size, with the suffixes of `crate::size`.
    Size,
    File,
    Folder,
    /// A file or a folder.
    Path,
    Command,
    /// One of these words.
    Choice(&'static [&'static str]),
    Service,
    User,
    App,
    Permission,
    Volume,
    Device,
}

impl Kind {
    /// The type that msh's `extern` writes for it.
    pub(crate) fn declared(self) -> String {
        match self {
            Kind::Text | Kind::Size => "string".into(),
            Kind::Int => "int".into(),
            Kind::File => "file".into(),
            Kind::Folder => "folder".into(),
            Kind::Path => "path".into(),
            Kind::Command => "command".into(),
            Kind::Choice(words) => format!("[{}]", words.join(" ")),
            Kind::Service => "service".into(),
            Kind::User => "user".into(),
            Kind::App => "app".into(),
            Kind::Permission => "permission".into(),
            Kind::Volume => "volume".into(),
            Kind::Device => "device".into(),
        }
    }
}

/// An option: its long name without `--`, empty when it has none, its
/// short form, and, when it takes one, the label and kind of its value.
#[derive(Debug, PartialEq)]
pub struct Opt {
    pub long: &'static str,
    pub short: Option<char>,
    pub value: Option<(&'static str, Kind)>,
    pub help: &'static str,
}

impl Opt {
    /// An option with only a short form.
    pub const fn short(short: char, help: &'static str) -> Opt {
        Opt {
            long: "",
            short: Some(short),
            value: None,
            help,
        }
    }

    /// An option with only a long form.
    pub const fn long(long: &'static str, help: &'static str) -> Opt {
        Opt {
            long,
            short: None,
            value: None,
            help,
        }
    }

    /// An option with both forms.
    pub const fn both(long: &'static str, short: char, help: &'static str) -> Opt {
        Opt {
            long,
            short: Some(short),
            value: None,
            help,
        }
    }

    /// The option, taking a value of `kind` shown as `label`.
    pub const fn takes(self, label: &'static str, kind: Kind) -> Opt {
        Opt {
            value: Some((label, kind)),
            ..self
        }
    }

    /// Whether `name`, `-x` or `--long`, names it.
    pub fn named(&self, name: &str) -> bool {
        match name.strip_prefix("--") {
            Some(long) => !self.long.is_empty() && long == self.long,
            None => {
                let mut chars = name.strip_prefix('-').unwrap_or("").chars();
                self.short.is_some() && chars.next() == self.short && chars.next().is_none()
            }
        }
    }

    /// How `--help` and errors name it: `-x, --long` or either.
    pub(crate) fn shown(&self) -> String {
        let mut text = match (self.short, self.long) {
            (Some(s), "") => format!("-{s}"),
            (Some(s), long) => format!("-{s}, --{long}"),
            (None, long) => format!("--{long}"),
        };
        if let Some((label, _)) = self.value {
            let _ = write!(text, " <{label}>");
        }
        text
    }

    /// The name that errors give: `--long`, or `-x` without one.
    fn name(&self) -> String {
        match self.short {
            Some(s) if self.long.is_empty() => format!("-{s}"),
            _ => format!("--{}", self.long),
        }
    }

    /// The item of msh's `extern` for it.
    pub(crate) fn declared(&self) -> String {
        let kind = self.value.map_or("bool".into(), |(_, k)| k.declared());
        let name = match (self.short, self.long) {
            (Some(s), "") => format!("-{s}"),
            (Some(s), long) => format!("--{long}(-{s})"),
            (None, long) => format!("--{long}"),
        };
        format!("{name}: {kind} \"{}\"", self.help)
    }
}

/// An operand: shown as `label`, of `kind`; `optional`, or taking all
/// the rest when `rest`.
#[derive(Debug)]
pub struct Operand {
    pub label: &'static str,
    pub kind: Kind,
    pub optional: bool,
    pub rest: bool,
    pub help: &'static str,
}

impl Operand {
    pub const fn one(label: &'static str, kind: Kind, help: &'static str) -> Operand {
        Operand {
            label,
            kind,
            optional: false,
            rest: false,
            help,
        }
    }

    pub const fn optional(label: &'static str, kind: Kind, help: &'static str) -> Operand {
        Operand {
            optional: true,
            ..Operand::one(label, kind, help)
        }
    }

    /// All remaining operands, at least one.
    pub const fn some(label: &'static str, kind: Kind, help: &'static str) -> Operand {
        Operand {
            rest: true,
            ..Operand::one(label, kind, help)
        }
    }

    /// All remaining operands, maybe none.
    pub const fn any(label: &'static str, kind: Kind, help: &'static str) -> Operand {
        Operand {
            rest: true,
            optional: true,
            ..Operand::one(label, kind, help)
        }
    }

    pub(crate) fn shown(&self) -> String {
        match (self.optional, self.rest) {
            (false, false) => format!("<{}>", self.label),
            (true, false) => format!("[{}]", self.label),
            (false, true) => format!("<{}>...", self.label),
            (true, true) => format!("[{}...]", self.label),
        }
    }

    pub(crate) fn declared(&self) -> String {
        let label = match (self.optional, self.rest) {
            (_, true) => format!("...{}", self.label),
            (true, false) => format!("{}?", self.label),
            (false, false) => self.label.into(),
        };
        format!("{label}: {} \"{}\"", self.kind.declared(), self.help)
    }
}

/// A command or subcommand: its name, what it does, and what it takes.
/// The options of a command also apply to its subcommands.
#[derive(Debug)]
pub struct Spec {
    pub name: &'static str,
    pub about: &'static str,
    pub operands: &'static [Operand],
    pub options: &'static [Opt],
    pub subcommands: &'static [Spec],
    /// Whether options end at the first operand, for commands whose
    /// operands are a command to run.
    pub stops: bool,
    /// Whether `--help` that does not fit the terminal goes through
    /// less: for every command but less itself.
    pub pages_help: bool,
    /// Which arguments that start with `-` are operands all the same,
    /// such as chmod's `-w`; none when absent.
    pub dash_operand: Option<fn(&str) -> bool>,
}

impl Spec {
    /// A command without operands, options or subcommands yet.
    pub const fn new(name: &'static str, about: &'static str) -> Spec {
        Spec {
            name,
            about,
            operands: &[],
            options: &[],
            subcommands: &[],
            stops: false,
            pages_help: true,
            dash_operand: None,
        }
    }
}

/// The options that every utility takes besides its own.
pub static HELP: Opt = Opt::long("help", "show how to use the command and exit");
pub static COLOR: Opt = Opt::long("color", "colour the output: auto, always or never")
    .takes("when", Kind::Choice(&["auto", "always", "never"]));
pub(crate) static COMMON: [&Opt; 2] = [&HELP, &COLOR];

/// The arguments of a run that is to go ahead.
#[derive(Debug, PartialEq)]
pub struct Run {
    /// The subcommands given, from the outermost.
    pub subcommands: Vec<&'static str>,
    /// The options given, in order, with their values.
    given: Vec<(&'static Opt, Option<String>)>,
    pub operands: Vec<String>,
}

impl Run {
    /// Whether the option `name` was given.
    pub fn has(&self, name: &str) -> bool {
        self.given.iter().any(|(o, _)| o.named(name))
    }

    /// How often the option `name` was given.
    pub fn count(&self, name: &str) -> usize {
        self.given.iter().filter(|(o, _)| o.named(name)).count()
    }

    /// The last value given with the option `name`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values(name).last().copied()
    }

    /// Every value given with the option `name`, in order.
    pub fn values(&self, name: &str) -> Vec<&str> {
        self.given
            .iter()
            .filter(|(o, _)| o.named(name))
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    /// Which of `names` was given last, for options of which the last
    /// one wins.
    pub fn last_of<'a>(&self, names: &[&'a str]) -> Option<&'a str> {
        self.given
            .iter()
            .rev()
            .find_map(|(o, _)| names.iter().find(|n| o.named(n)).copied())
    }

    /// The subcommand given right below the command, if any.
    pub fn subcommand(&self) -> Option<&'static str> {
        self.subcommands.first().copied()
    }
}

/// What the arguments ask for.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Run),
    /// `--help`, for the command or subcommands given.
    Help(Vec<&'static str>),
    /// Invalid arguments, for the reason given.
    Invalid(String),
}

impl Spec {
    /// The command or subcommand below this one that `path` names.
    pub fn below(&'static self, path: &[&str]) -> &'static Spec {
        let mut at = self;
        for name in path {
            match at.subcommands.iter().find(|s| s.name == *name) {
                Some(sub) => at = sub,
                None => break,
            }
        }
        at
    }

    /// The options that apply at `path`: the innermost of each name, then
    /// the common ones.
    pub(crate) fn options_at(&'static self, path: &[&str]) -> Vec<&'static Opt> {
        let mut levels = vec![self];
        let mut at = self;
        for name in path {
            if let Some(sub) = at.subcommands.iter().find(|s| s.name == *name) {
                levels.push(sub);
                at = sub;
            }
        }
        let mut options: Vec<&'static Opt> = Vec::new();
        for level in levels.iter().rev() {
            for o in level.options {
                let clash = options.iter().any(|p| {
                    (!o.long.is_empty() && p.long == o.long)
                        || (o.short.is_some() && p.short == o.short)
                });
                if !clash {
                    options.push(o);
                }
            }
        }
        options.extend(COMMON);
        options
    }

    /// Reads `arguments`, those after the command's name, as
    /// MIKSPEC-0098.R31 says: grouped short options, values after the
    /// option or as the next argument, `--` and `-`, and subcommands
    /// before the first operand.
    pub fn parse(&'static self, arguments: impl IntoIterator<Item = String>) -> Parsed {
        let mut path: Vec<&'static str> = Vec::new();
        let mut given: Vec<(&'static Opt, Option<String>)> = Vec::new();
        let mut operands: Vec<String> = Vec::new();
        let mut ended = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            let here = self.below(&path);
            let operand_anyway = here.dash_operand.is_some_and(|is| is(&argument));
            if ended || argument == "-" || !argument.starts_with('-') || operand_anyway {
                let sub = here.subcommands.iter().find(|s| s.name == argument);
                match sub {
                    Some(sub) if operands.is_empty() && !ended => path.push(sub.name),
                    _ => {
                        operands.push(argument);
                        ended |= here.stops;
                    }
                }
                continue;
            }
            if argument == "--" {
                ended = true;
                continue;
            }
            let options = self.options_at(&path);
            if let Some(long) = argument.strip_prefix("--") {
                let (name, inline) = match long.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (long, None),
                };
                let Some(&option) = options
                    .iter()
                    .find(|o| !o.long.is_empty() && o.long == name)
                else {
                    return Parsed::Invalid(format!("unknown option --{name}"));
                };
                let value = match (option.value, inline) {
                    (None, Some(_)) => {
                        return Parsed::Invalid(format!("option --{name} takes no value"));
                    }
                    (None, None) => None,
                    (Some(_), Some(value)) => Some(value),
                    (Some(_), None) => match arguments.next() {
                        Some(value) => Some(value),
                        None => return Parsed::Invalid(format!("option --{name} needs a value")),
                    },
                };
                given.push((option, value));
                continue;
            }
            let shorts: Vec<char> = argument[1..].chars().collect();
            let mut i = 0;
            while i < shorts.len() {
                let c = shorts[i];
                let Some(&option) = options.iter().find(|o| o.short == Some(c)) else {
                    return Parsed::Invalid(format!("unknown option -{c}"));
                };
                if option.value.is_none() {
                    given.push((option, None));
                    i += 1;
                    continue;
                }
                let rest: String = shorts[i + 1..].iter().collect();
                let value = match rest.is_empty() {
                    false => rest,
                    true => match arguments.next() {
                        Some(value) => value,
                        None => return Parsed::Invalid(format!("option -{c} needs a value")),
                    },
                };
                given.push((option, Some(value)));
                break;
            }
        }
        if given.iter().any(|(o, _)| std::ptr::eq(*o, &HELP)) {
            return Parsed::Help(path);
        }
        for (option, value) in &given {
            if let (Some((_, Kind::Choice(words))), Some(value)) = (option.value, value)
                && !words.contains(&value.as_str())
            {
                return Parsed::Invalid(format!("invalid value {value} for {}", option.name()));
            }
        }
        let here = self.below(&path);
        let least = here.operands.iter().filter(|o| !o.optional).count();
        let most = match here.operands.iter().any(|o| o.rest) {
            true => usize::MAX,
            false => here.operands.len(),
        };
        if operands.len() < least {
            let missing = &here.operands[operands.len().min(here.operands.len() - 1)];
            return Parsed::Invalid(format!("missing operand {}", missing.shown()));
        }
        if operands.len() > most {
            return Parsed::Invalid(format!("unexpected argument {}", operands[most]));
        }
        Parsed::Run(Run {
            subcommands: path,
            given,
            operands,
        })
    }

    /// The options at `path`, for completion.
    pub(crate) fn options_for(&'static self, path: &[&str]) -> Vec<&'static Opt> {
        self.options_at(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static LIST: Spec = Spec {
        operands: &[Operand::any("path", Kind::Path, "what to list")],
        options: &[
            Opt::short('l', "long lines"),
            Opt::both("all", 'a', "show hidden entries"),
            Opt::short('n', "at most count").takes("count", Kind::Int),
            Opt::long("sort", "the order").takes("key", Kind::Choice(&["name", "size"])),
            Opt::long("json", "write JSON"),
        ],
        ..Spec::new("list", "Lists what there is.")
    };

    static TOOL: Spec = Spec {
        options: &[Opt::long("json", "write JSON")],
        subcommands: &[
            Spec {
                operands: &[Operand::one("name", Kind::Service, "the service")],
                options: &[Opt::both("force", 'f', "do it anyway")],
                ..Spec::new("stop", "Stops a service.")
            },
            Spec::new("list", "Lists the services."),
        ],
        ..Spec::new("tool", "Does things.")
    };

    static RUN: Spec = Spec {
        operands: &[Operand::some("command", Kind::Command, "what to run")],
        options: &[Opt::short('i', "start empty")],
        stops: true,
        ..Spec::new("run", "Runs a command.")
    };

    fn parse(spec: &'static Spec, arguments: &[&str]) -> Parsed {
        spec.parse(arguments.iter().map(|a| a.to_string()))
    }

    fn run(spec: &'static Spec, arguments: &[&str]) -> Run {
        match parse(spec, arguments) {
            Parsed::Run(run) => run,
            other => panic!("{arguments:?} gave {other:?}"),
        }
    }

    #[test]
    fn reads_grouped_options_values_and_operands() {
        let r = run(&LIST, &["-la", "x", "-n5", "--sort=size", "y"]);
        assert!(r.has("-l") && r.has("-a") && r.has("--all"));
        assert_eq!(r.value("-n"), Some("5"));
        assert_eq!(r.value("--sort"), Some("size"));
        assert_eq!(r.operands, ["x", "y"]);
        let r = run(&LIST, &["-n", "7", "--sort", "name", "-", "--", "-l"]);
        assert_eq!(r.values("-n"), ["7"]);
        assert_eq!(r.operands, ["-", "-l"]);
        assert!(!r.has("-l"));
        let r = run(&LIST, &["-l", "-l", "--color=never"]);
        assert_eq!(r.count("-l"), 2);
        assert_eq!(r.value("--color"), Some("never"));
        let r = run(&LIST, &["-l", "--all", "-l"]);
        assert_eq!(r.last_of(&["-a", "-l"]), Some("-l"));
        assert_eq!(r.last_of(&["--all"]), Some("--all"));
        assert_eq!(r.last_of(&["-n"]), None);
    }

    #[test]
    fn names_bad_arguments() {
        let invalid =
            |a: &[&str], why: &str| assert_eq!(parse(&LIST, a), Parsed::Invalid(why.into()));
        invalid(&["-x"], "unknown option -x");
        invalid(&["-lx"], "unknown option -x");
        invalid(&["--nope"], "unknown option --nope");
        invalid(&["-n"], "option -n needs a value");
        invalid(&["--sort"], "option --sort needs a value");
        invalid(&["--json=1"], "option --json takes no value");
        invalid(&["--sort=age"], "invalid value age for --sort");
        invalid(&["--color=red"], "invalid value red for --color");
        assert_eq!(
            parse(&LIST, &["-x", "--help"]),
            Parsed::Invalid("unknown option -x".into())
        );
        assert_eq!(parse(&LIST, &["-l", "--help"]), Parsed::Help(vec![]));
    }

    #[test]
    fn reads_subcommands_with_their_options() {
        let r = run(&TOOL, &["stop", "-f", "logsvc", "--json"]);
        assert_eq!(r.subcommand(), Some("stop"));
        assert!(r.has("--force") && r.has("--json"));
        assert_eq!(r.operands, ["logsvc"]);
        assert_eq!(
            parse(&TOOL, &["-f", "stop", "x"]),
            Parsed::Invalid("unknown option -f".into())
        );
        assert_eq!(
            parse(&TOOL, &["stop"]),
            Parsed::Invalid("missing operand <name>".into())
        );
        assert_eq!(
            parse(&TOOL, &["stop", "a", "b"]),
            Parsed::Invalid("unexpected argument b".into())
        );
        assert_eq!(
            parse(&TOOL, &["list", "x"]),
            Parsed::Invalid("unexpected argument x".into())
        );
        assert_eq!(
            parse(&TOOL, &["stop", "--help"]),
            Parsed::Help(vec!["stop"])
        );
        assert_eq!(run(&TOOL, &[]).subcommand(), None);
    }

    static MODE: Spec = Spec {
        operands: &[Operand::some("arg", Kind::Text, "modes and paths")],
        options: &[Opt::short('R', "recursively")],
        dash_operand: Some(|a| a.chars().skip(1).all(|c| "rwx".contains(c))),
        ..Spec::new("mode", "Sets modes.")
    };

    #[test]
    fn takes_operands_that_look_like_options() {
        let r = run(&MODE, &["-R", "-w", "f"]);
        assert!(r.has("-R"));
        assert_eq!(r.operands, ["-w", "f"]);
        assert_eq!(
            parse(&MODE, &["-q", "f"]),
            Parsed::Invalid("unknown option -q".into())
        );
    }

    #[test]
    fn stops_at_the_command_to_run() {
        let r = run(&RUN, &["-i", "ls", "-l", "--", "x"]);
        assert!(r.has("-i"));
        assert_eq!(r.operands, ["ls", "-l", "--", "x"]);
        assert_eq!(
            parse(&RUN, &["-i"]),
            Parsed::Invalid("missing operand <command>...".into())
        );
    }
}
