//! The shell as the source of what Tab completes: its functions with
//! their flags, its variables and environment, the programs of
//! `$env.PATH`, folders, and the options that scripts declare.

use super::{Shell, System};
use crate::ast::{Param, StatementKind};
use crate::edit::complete::Sources;
use crate::value::{Value, scalar_text};

impl<S: System> Sources for Shell<S> {
    fn entries(&self, folder: &str) -> Option<Vec<(String, bool)>> {
        self.system.entries(folder)
    }

    fn programs(&self) -> Vec<String> {
        let Some(Value::List(folders)) = self.environment.get("PATH") else {
            return Vec::new();
        };
        let mut programs = Vec::new();
        for folder in folders.iter().map(scalar_text) {
            for (name, is_folder) in self.system.entries(&folder).unwrap_or_default() {
                let path = format!("{}/{name}", folder.trim_end_matches('/'));
                if !is_folder && self.system.executable(&path) == Some(true) {
                    programs.push(name);
                }
            }
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
