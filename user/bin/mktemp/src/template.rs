//! What mktemp takes, where it makes its file or folder, and how it
//! fills the X at the end of a template (MIKSPEC-0098.R41). The folder
//! is the one of `-p`, or else `$TMPDIR`, or else `/tmp` (MIKSPEC-0050.R43);
//! a template with a `/` and neither `-p` nor `-t` is a path of its own.

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::path::PathBuf;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::optional(
        "template",
        Kind::Text,
        "the name to make, ending in at least three X",
    )],
    options: &[
        Opt::both("directory", 'd', "make a folder, not a file"),
        Opt::both("dry-run", 'u', "make nothing, only write a name not taken"),
        Opt::both(
            "quiet",
            'q',
            "say nothing when the file or folder cannot be made",
        ),
        Opt::both("tmpdir", 'p', "make it in this folder").takes("folder", Kind::Folder),
        Opt::short('t', "take the template as a name without folders"),
        Opt::long("suffix", "add this after the X of the template").takes("text", Kind::Text),
    ],
    ..Spec::new(
        "mktemp",
        "Makes a file, or a folder with -d, whose name is the template with its\n\
         X replaced by letters and digits, and writes its path. The template is\n\
         tmp.XXXXXXXXXX when absent; the folder is that of -p, or $TMPDIR, or /tmp.",
    )
};

/// The template when none is given.
pub const DEFAULT: &str = "tmp.XXXXXXXXXX";
/// The fewest X that a template ends in.
const LEAST: usize = 3;
/// What the X become.
const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Where and from what a name is made.
#[derive(Debug, PartialEq)]
pub struct Plan {
    /// The folder of the name, or none for a template that is a path.
    pub folder: Option<PathBuf>,
    /// The template before its X.
    pub head: String,
    /// How many X it ends in.
    pub xs: usize,
    /// What follows the X, from `--suffix`.
    pub suffix: String,
}

impl Plan {
    /// The path of a name with each X replaced by a letter or digit
    /// from `random`, or the template's own with `random` absent.
    pub fn path(&self, random: Option<&mut dyn FnMut() -> u64>) -> PathBuf {
        let filled: String = match random {
            Some(random) => (0..self.xs)
                .map(|_| char::from(LETTERS[(random() % LETTERS.len() as u64) as usize]))
                .collect(),
            None => "X".repeat(self.xs),
        };
        let name = format!("{}{filled}{}", self.head, self.suffix);
        match &self.folder {
            Some(folder) => folder.join(name),
            None => PathBuf::from(name),
        }
    }
}

/// Where the run makes its name and from what, with `tmpdir` the value
/// of `$TMPDIR`, or why its arguments are invalid.
pub fn plan(run: &Run, tmpdir: Option<&str>) -> Result<Plan, String> {
    let given = run.operands.first().map(String::as_str);
    let template = given.unwrap_or(DEFAULT);
    let suffix = run.value("--suffix").unwrap_or_default();
    if suffix.contains('/') {
        return Err(format!("the suffix {suffix} has a /"));
    }
    let head = template.trim_end_matches('X');
    let xs = template.len() - head.len();
    if xs < LEAST {
        return Err(format!(
            "the template {template} does not end in at least {LEAST} X"
        ));
    }
    let chosen = run.value("-p").filter(|p| !p.is_empty());
    let temporary = || {
        let tmpdir = tmpdir.filter(|t| !t.is_empty()).unwrap_or("/tmp");
        Some(PathBuf::from(chosen.unwrap_or(tmpdir)))
    };
    let folder = if run.has("-t") {
        if template.contains('/') {
            return Err(format!("with -t, the template {template} must have no /"));
        }
        temporary()
    } else if chosen.is_some() {
        if template.starts_with('/') {
            return Err(format!(
                "with -p, the template {template} must not be absolute"
            ));
        }
        temporary()
    } else if given.is_some_and(|t| t.contains('/')) {
        None
    } else {
        temporary()
    };
    Ok(Plan {
        folder,
        head: head.into(),
        xs,
        suffix: suffix.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn plan_of(arguments: &[&str], tmpdir: Option<&str>) -> Result<Plan, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => plan(&run, tmpdir),
            other => panic!("{other:?}"),
        }
    }

    fn folder(arguments: &[&str], tmpdir: Option<&str>) -> Option<PathBuf> {
        plan_of(arguments, tmpdir).unwrap().folder
    }

    #[test]
    fn makes_names_in_p_or_tmpdir_or_tmp() {
        let at = |p: &str| Some(PathBuf::from(p));
        assert_eq!(folder(&[], None), at("/tmp"));
        assert_eq!(folder(&[], Some("")), at("/tmp"));
        assert_eq!(folder(&[], Some("/t")), at("/t"));
        assert_eq!(folder(&["a.XXX"], Some("/t")), at("/t"));
        assert_eq!(folder(&["-p", "/p", "a.XXX"], Some("/t")), at("/p"));
        assert_eq!(folder(&["-t", "a.XXX"], Some("/t")), at("/t"));
        assert_eq!(folder(&["-t", "-p", "/p", "a.XXX"], Some("/t")), at("/p"));
        assert_eq!(folder(&["-p", "/p", "d/a.XXX"], None), at("/p"));
        assert_eq!(folder(&["d/a.XXX"], Some("/t")), None);
        assert_eq!(folder(&["/d/a.XXX"], Some("/t")), None);
    }

    #[test]
    fn refuses_bad_templates() {
        let why = |a: &[&str]| plan_of(a, None).unwrap_err();
        assert_eq!(
            why(&["aXX"]),
            "the template aXX does not end in at least 3 X"
        );
        assert_eq!(
            why(&["aXXXb"]),
            "the template aXXXb does not end in at least 3 X"
        );
        assert_eq!(
            why(&["-t", "d/aXXX"]),
            "with -t, the template d/aXXX must have no /"
        );
        assert_eq!(
            why(&["-p", "/p", "/aXXX"]),
            "with -p, the template /aXXX must not be absolute"
        );
        assert_eq!(why(&["--suffix", "a/b"]), "the suffix a/b has a /");
    }

    #[test]
    fn fills_every_x_and_keeps_the_rest() {
        let plan = plan_of(&["--suffix=.txt", "-p", "/p", "aXbXXXX"], None).unwrap();
        assert_eq!((plan.head.as_str(), plan.xs), ("aXb", 4));
        assert_eq!(plan.path(None), PathBuf::from("/p/aXbXXXX.txt"));
        let mut n = 0u64;
        let mut next = || {
            n += 1;
            n * 26
        };
        assert_eq!(plan.path(Some(&mut next)), PathBuf::from("/p/aXba0Qq.txt"));
        let plan = plan_of(&[], Some("/t")).unwrap();
        let path = plan.path(Some(&mut || 61)).to_string_lossy().into_owned();
        assert_eq!(path, "/t/tmp.9999999999");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/mktemp.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
