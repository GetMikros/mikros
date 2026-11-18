//! What du takes, and how it writes the space of each path: a size and a
//! path on each line, or JSON (MIKSPEC-0098.R39).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string, size};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "the files and folders to measure, or the current folder",
    )],
    options: &[
        Opt::both("all", 'a', "show files too, not only folders"),
        Opt::both("summarize", 's', "show only a total for each path"),
        Opt::both("total", 'c', "show a grand total at the end"),
        Opt::both("human-readable", 'h', "show sizes with K, M and G"),
        Opt::both(
            "max-depth",
            'd',
            "show folders at most <depth> below each path",
        )
        .takes("depth", Kind::Int),
        Opt::long(
            "apparent-size",
            "count the sizes of the files, not the space they use",
        ),
        Opt::short('k', "show sizes in units of 1024 bytes, as without options"),
        Opt::short('m', "show sizes in units of 1024 * 1024 bytes"),
        Opt::long("json", "write the paths as a JSON array of objects"),
    ],
    ..Spec::new(
        "du",
        "Shows the space that each path and the folders below it use, counting\n\
         each file once however many links it has.",
    )
};

/// How sizes are shown in text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Kibibytes,
    Mebibytes,
    Human,
}

/// What a run asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Show {
    pub all: bool,
    pub total: bool,
    /// The deepest level shown, 0 for the paths themselves.
    pub depth: Option<usize>,
    pub apparent: bool,
    pub unit: Unit,
    pub json: bool,
}

impl Show {
    /// What `run` asks, or why its arguments are invalid.
    pub fn of(run: &Run) -> Result<Show, String> {
        let depth = match run.value("-d") {
            Some(text) => Some(
                text.parse::<usize>()
                    .map_err(|_| format!("invalid depth {text}"))?,
            ),
            None => None,
        };
        let summarize = run.has("-s");
        if summarize && run.has("-a") {
            return Err("-s and -a cannot be given together".into());
        }
        if summarize && depth.is_some_and(|d| d != 0) {
            return Err("-s and -d cannot be given together".into());
        }
        let unit = if run.has("-h") {
            Unit::Human
        } else if run.has("-m") {
            Unit::Mebibytes
        } else {
            Unit::Kibibytes
        };
        Ok(Show {
            all: run.has("-a"),
            total: run.has("-c"),
            depth: if summarize { Some(0) } else { depth },
            apparent: run.has("--apparent-size"),
            unit,
            json: run.has("--json"),
        })
    }

    /// Whether an entry `depth` below its path is shown, a folder when
    /// `folder`.
    pub fn shows(&self, depth: usize, folder: bool) -> bool {
        (folder || self.all || depth == 0) && self.depth.is_none_or(|most| depth <= most)
    }
}

/// `bytes` as `unit` shows them, whole units rounded up.
pub fn shown(bytes: u64, unit: Unit) -> String {
    match unit {
        Unit::Kibibytes => bytes.div_ceil(1 << 10).to_string(),
        Unit::Mebibytes => bytes.div_ceil(1 << 20).to_string(),
        Unit::Human => size::human(bytes),
    }
}

/// A line of text: the size, a tab and the path.
pub fn line(bytes: u64, path: &str, unit: Unit, paint: Paint) -> String {
    format!(
        "{}\t{}\n",
        paint.paint(Role::Number, &shown(bytes, unit)),
        paint.paint(Role::Path, path)
    )
}

/// The paths and their bytes as a JSON array of objects with `path` and
/// `size`, on one line.
pub fn json(entries: &[(String, u64)]) -> String {
    let objects: Vec<String> = entries
        .iter()
        .map(|(path, bytes)| format!("{{\"path\":{},\"size\":{bytes}}}", json_string(path)))
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn show(arguments: &[&str]) -> Result<Show, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Show::of(&run),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_what_to_show() {
        let plain = show(&[]).unwrap();
        assert_eq!(plain.unit, Unit::Kibibytes);
        assert_eq!(plain.depth, None);
        assert!(plain.shows(5, true) && !plain.shows(1, false) && plain.shows(0, false));
        let summary = show(&["-s"]).unwrap();
        assert!(summary.shows(0, true) && !summary.shows(1, true));
        let deep = show(&["-a", "-d", "1"]).unwrap();
        assert!(deep.shows(1, false) && !deep.shows(2, true));
        assert_eq!(show(&["-m"]).unwrap().unit, Unit::Mebibytes);
        assert_eq!(show(&["-mh"]).unwrap().unit, Unit::Human);
        assert!(show(&["-c", "--apparent-size"]).unwrap().apparent);
    }

    #[test]
    fn refuses_conflicts_and_bad_depths() {
        assert!(show(&["-s", "-a"]).is_err());
        assert!(show(&["-s", "-d", "2"]).is_err());
        assert!(show(&["-s", "-d", "0"]).is_ok());
        assert_eq!(show(&["-d", "x"]), Err("invalid depth x".into()));
    }

    #[test]
    fn writes_sizes() {
        assert_eq!(shown(0, Unit::Kibibytes), "0");
        assert_eq!(shown(1, Unit::Kibibytes), "1");
        assert_eq!(shown(4096, Unit::Kibibytes), "4");
        assert_eq!(shown(4096, Unit::Mebibytes), "1");
        assert_eq!(shown(4096, Unit::Human), "4.0K");
        assert_eq!(
            line(8192, "a/b", Unit::Kibibytes, Paint::never()),
            "8\ta/b\n"
        );
        assert_eq!(
            json(&[("a".into(), 4096), ("total".into(), 4096)]),
            "[{\"path\":\"a\",\"size\":4096},{\"path\":\"total\",\"size\":4096}]\n"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/du.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
