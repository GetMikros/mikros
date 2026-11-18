//! The text of `--help`, and the declarations for msh's completion
//! (MIKSPEC-0084.R67), both from the tables of `spec`.

use crate::spec::{COMMON, Operand, Spec};
use std::fmt::Write;

impl Spec {
    /// The text of `--help` for the command or the subcommands of `path`:
    /// the usage, what it does, its subcommands and its options.
    pub fn help(&'static self, path: &[&str]) -> String {
        let here = self.below(path);
        let mut usage = vec![self.name.to_string()];
        usage.extend(path.iter().map(|p| p.to_string()));
        let mut text = format!("Usage: {} [options]", usage.join(" "));
        if !here.subcommands.is_empty() {
            text.push_str(" <subcommand>");
        }
        for o in here.operands {
            let _ = write!(text, " {}", o.shown());
        }
        let _ = write!(text, "\n\n{}\n", here.about);
        let column = |rows: &[(String, &str)], text: &mut String| {
            let width = rows
                .iter()
                .map(|(n, _)| n.chars().count())
                .max()
                .unwrap_or(0);
            for (name, what) in rows {
                let _ = writeln!(text, "  {name:width$}  {what}");
            }
        };
        if !here.subcommands.is_empty() {
            text.push_str("\nSubcommands:\n");
            let rows: Vec<(String, &str)> = here
                .subcommands
                .iter()
                .map(|s| (s.name.to_string(), s.about.lines().next().unwrap_or("")))
                .collect();
            column(&rows, &mut text);
        }
        text.push_str("\nOptions:\n");
        let rows: Vec<(String, &str)> = self
            .options_at(path)
            .iter()
            .map(|o| (o.shown(), o.help))
            .collect();
        column(&rows, &mut text);
        text
    }

    /// The declarations of the command and of every subcommand for msh
    /// (MIKSPEC-0084.R67), as the command's completion file holds them,
    /// each ending in `complete`, so that msh asks the command itself for
    /// candidates (MIKSPEC-0084.R69).
    pub fn declaration(&'static self) -> String {
        let mut text = format!(
            "# What `{}` takes, for msh's completion and colours. The command\n\
             # answers completion itself, from the same tables.\n",
            self.name
        );
        let mut levels: Vec<(String, &'static Spec)> = vec![(self.name.into(), self)];
        let mut i = 0;
        while i < levels.len() {
            let (name, spec) = (levels[i].0.clone(), levels[i].1);
            let mut items: Vec<String> = spec.operands.iter().map(Operand::declared).collect();
            if i == 0 {
                items.extend(self.options.iter().map(|o| o.declared()));
                items.extend(COMMON.iter().map(|o| o.declared()));
            } else {
                items.extend(spec.options.iter().map(|o| o.declared()));
            }
            let _ = write!(text, "extern {name}(");
            match items.is_empty() {
                true => text.push(')'),
                false => {
                    text.push('\n');
                    let _ = write!(text, "    {}", items.join(",\n    "));
                    text.push_str("\n)");
                }
            }
            text.push_str(" complete\n");
            for sub in spec.subcommands {
                levels.push((format!("{name} {}", sub.name), sub));
            }
            i += 1;
        }
        text
    }
}

/// For a command's tests: whether its completion file at `path` holds
/// what its tables declare, as `Err` with what it should hold otherwise.
/// With the environment variable `MIKROS_BLESS` set to 1, writes the
/// file instead.
pub fn declared(spec: &'static Spec, path: &std::path::Path) -> Result<(), String> {
    let wanted = spec.declaration();
    if std::env::var("MIKROS_BLESS").is_ok_and(|v| v == "1") {
        return std::fs::write(path, &wanted)
            .map_err(|e| format!("writing {}: {e}", path.display()));
    }
    match std::fs::read_to_string(path) {
        Ok(text) if text == wanted => Ok(()),
        Ok(_) => Err(format!(
            "{} differs from the tables; MIKROS_BLESS=1 rewrites it:\n{wanted}",
            path.display()
        )),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use crate::spec::{Kind, Operand, Opt, Spec};

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

    #[test]
    fn writes_help() {
        assert_eq!(
            LIST.help(&[]),
            "Usage: list [options] [path...]\n\nLists what there is.\n\nOptions:\n  \
             -l              long lines\n  \
             -a, --all       show hidden entries\n  \
             -n <count>      at most count\n  \
             --sort <key>    the order\n  \
             --json          write JSON\n  \
             --help          show how to use the command and exit\n  \
             --color <when>  colour the output: auto, always or never\n"
        );
        let help = TOOL.help(&[]);
        assert!(help.starts_with("Usage: tool [options] <subcommand>\n\nDoes things.\n\nSubcommands:\n  stop  Stops a service.\n  list  Lists the services.\n"));
        assert!(
            TOOL.help(&["stop"])
                .starts_with("Usage: tool stop [options] <name>\n")
        );
        assert!(TOOL.help(&["stop"]).contains("-f, --force"));
    }

    #[test]
    fn declares_every_level() {
        assert_eq!(
            TOOL.declaration(),
            "# What `tool` takes, for msh's completion and colours. The command\n\
             # answers completion itself, from the same tables.\n\
             extern tool(\n    \
             --json: bool \"write JSON\",\n    \
             --help: bool \"show how to use the command and exit\",\n    \
             --color: [auto always never] \"colour the output: auto, always or never\"\n\
             ) complete\n\
             extern tool stop(\n    \
             name: service \"the service\",\n    \
             --force(-f): bool \"do it anyway\"\n\
             ) complete\n\
             extern tool list() complete\n"
        );
        let list = LIST.declaration();
        assert!(
            list.contains("    ...path: path \"what to list\",\n    -l: bool \"long lines\",\n")
        );
        assert!(
            list.contains(
                "    -n: int \"at most count\",\n    --sort: [name size] \"the order\",\n"
            )
        );
    }
}
