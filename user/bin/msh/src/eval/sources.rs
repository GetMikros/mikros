//! The shell as the source of what Tab completes: its functions with
//! their flags, its variables and environment, the programs of
//! `$env.PATH`, folders, and the options that scripts declare. Folders
//! are listed through the listings the shell keeps between keys.

use super::listings::Disk;
use super::{Shell, System};
use crate::ast::{Extern, Param, StatementKind};
use crate::edit::complete::Sources;
use crate::value::{Value, scalar_text};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

impl<S: System> Shell<S> {
    /// Lists folders anew for the next line, keeping the programs of
    /// `PATH`, which are listed again once their folder changes.
    pub fn new_line(&self) {
        self.listings.forget_entries();
    }

    /// Starts the thread that reads folders on `disk` for suggestions,
    /// which calls `changed` when it read something new.
    pub fn read_folders_in_background(
        &mut self,
        disk: Arc<dyn Disk>,
        changed: impl Fn() + Send + 'static,
    ) {
        self.listings.start_reader(disk, changed);
    }

    /// The shell as the source of a suggestion, which does not wait for
    /// folders to be read once the reader thread runs.
    pub fn without_waiting(&self) -> WithoutWaiting<'_, S> {
        WithoutWaiting(self)
    }

    /// The folders of `$env.PATH`, in order.
    fn path_folders(&self) -> Vec<String> {
        match self.environment.get("PATH") {
            Some(Value::List(folders)) => folders.iter().map(scalar_text).collect(),
            _ => Vec::new(),
        }
    }

    /// The programs in `folder`, read now.
    fn read_programs(&self, folder: &str) -> Vec<String> {
        let mut found = Vec::new();
        for (name, is_folder) in self.system.entries(folder).unwrap_or_default() {
            let path = format!("{}/{name}", folder.trim_end_matches('/'));
            if !is_folder && self.system.executable(&path) == Some(true) {
                found.push(name);
            }
        }
        found
    }

    /// `folder` from the root, as the listings are kept.
    fn absolute(&self, folder: &str) -> String {
        if folder.starts_with('/') {
            return folder.to_owned();
        }
        let current = self.system.current_dir();
        match folder {
            "" | "." => current,
            f => format!("{}/{f}", current.trim_end_matches('/')),
        }
    }
}

impl<S: System> Sources for Shell<S> {
    fn declaration(&self, name: &str, sub: Option<&str>) -> Option<Rc<Extern>> {
        Shell::declaration(self, name, sub)
    }

    fn subcommands(&self, name: &str) -> Vec<String> {
        Shell::subcommands(self, name)
    }

    fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
        let path = self.absolute(folder);
        let modified = self.system.modified(&path);
        let now = SystemTime::now();
        self.listings
            .entries(&path, modified, now, || self.system.entries(folder))
    }

    fn programs(&self) -> Vec<String> {
        let now = SystemTime::now();
        let mut programs = Vec::new();
        for folder in self.path_folders() {
            let path = self.absolute(&folder);
            let modified = self.system.modified(&path);
            let read = || self.read_programs(&folder);
            programs.extend(self.listings.programs(&path, modified, now, read));
        }
        programs
    }

    fn functions(&self) -> Vec<(String, Vec<String>)> {
        self.functions
            .iter()
            .map(|(name, f)| {
                (
                    name.clone(),
                    f.flags.iter().map(|(n, _)| n.clone()).collect(),
                )
            })
            .collect()
    }

    fn variables(&self) -> Vec<(String, String)> {
        let mut variables: Vec<(String, String)> = Vec::new();
        for frame in self.frames.iter().rev() {
            for (name, value) in &frame.variables {
                if variables.iter().all(|(n, _)| n != name) {
                    variables.push((name.clone(), scalar_text(value)));
                }
            }
        }
        variables
    }

    fn environment(&self) -> Vec<(String, String)> {
        self.program_environment(&[])
    }

    fn current_folder(&self) -> Option<String> {
        Some(self.system.current_dir())
    }

    fn params(&self, path: &str) -> Option<Vec<Param>> {
        let source = self.system.read_file(path).ok()?;
        let block = crate::parse::parse(&source).ok()?;
        let params = block
            .statements
            .into_iter()
            .filter_map(|s| match s.kind {
                StatementKind::Param(p) => Some(p),
                _ => None,
            })
            .collect();
        Some(params)
    }
}

/// The shell as the source of a suggestion: with the reader thread
/// running, it takes the folders' kept listings and has the thread check
/// them, instead of reading them; it is the shell otherwise.
pub struct WithoutWaiting<'a, S: System>(&'a Shell<S>);

impl<S: System> Sources for WithoutWaiting<'_, S> {
    fn declaration(&self, name: &str, sub: Option<&str>) -> Option<Rc<Extern>> {
        self.0.declaration(name, sub)
    }

    fn subcommands(&self, name: &str) -> Vec<String> {
        self.0.subcommands(name)
    }

    fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
        let shell = self.0;
        match shell.listings.reading_in_background() {
            true => shell.listings.entries_now(&shell.absolute(folder)),
            false => Sources::entries(shell, folder),
        }
    }

    fn programs(&self) -> Vec<String> {
        let shell = self.0;
        if !shell.listings.reading_in_background() {
            return Sources::programs(shell);
        }
        let folders = shell.path_folders();
        let listed = |f: &String| shell.listings.programs_now(&shell.absolute(f));
        folders.iter().flat_map(listed).collect()
    }

    fn functions(&self) -> Vec<(String, Vec<String>)> {
        Sources::functions(self.0)
    }

    fn variables(&self) -> Vec<(String, String)> {
        Sources::variables(self.0)
    }

    fn environment(&self) -> Vec<(String, String)> {
        Sources::environment(self.0)
    }

    fn params(&self, path: &str) -> Option<Vec<Param>> {
        Sources::params(self.0, path)
    }

    fn current_folder(&self) -> Option<String> {
        Sources::current_folder(self.0)
    }
}
