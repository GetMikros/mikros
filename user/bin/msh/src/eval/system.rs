//! What the shell needs of the system, and the programs it starts.

use super::glob;
use crate::ast::RedirectionKind;
use crate::jobs::Ended;

/// A program to start.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// Its path, as found.
    pub path: String,
    /// Its arguments, its name first.
    pub args: Vec<String>,
    /// Its whole environment.
    pub environment: Vec<(String, String)>,
    pub redirections: Vec<(RedirectionKind, String)>,
}

/// Where the first program of a run reads from.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// The shell's standard input.
    Inherit,
    /// These bytes, then the end.
    Bytes(Vec<u8>),
}

/// Programs joined by pipes.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub programs: Vec<Program>,
    pub input: Input,
    /// The last program's output is returned rather than written to the
    /// shell's standard output.
    pub capture: bool,
    pub background: bool,
    /// The command as typed, for job control.
    pub text: String,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Ran {
    /// How each program ended; empty for a run started in the
    /// background or stopped.
    pub ended: Vec<Ended>,
    pub output: Vec<u8>,
}

/// The shell's standard output or error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Output,
    Error,
}

/// What the shell needs of the system.
pub trait System: glob::Folders {
    fn current_dir(&self) -> String;
    fn set_current_dir(&mut self, path: &str) -> Result<(), String>;
    fn read_file(&self, path: &str) -> Result<String, String>;
    /// Whether a file is at `path`, and if so whether it may be run.
    fn executable(&self, path: &str) -> Option<bool>;
    /// When the folder at `path` last changed, if the system tells; the
    /// shell then keeps its listing until it changes again.
    fn modified(&self, _path: &str) -> Option<std::time::SystemTime> {
        None
    }
    fn run(&mut self, run: Run) -> Result<Ran, String>;
    fn write(&mut self, stream: Stream, bytes: &[u8]);
    /// The jobs, one line each as `jobs` lists them.
    fn jobs(&mut self) -> Vec<String>;
    /// Continues job `id` (`%1`, `%+`, …), in the foreground, waiting
    /// for it to end or stop, or in the background.
    fn resume(&mut self, id: &str, foreground: bool) -> Result<Ran, String>;
    /// Sends `signal` to `target`, a job ID or a process ID.
    fn kill(&mut self, target: &str, signal: i32) -> Result<(), String>;
    /// Replaces the shell with `program`, returning only why it could not.
    fn exec(&mut self, program: Program) -> String;
}
