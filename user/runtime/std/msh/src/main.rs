//! msh, the Mikros shell: the syntax tree of its command language and
//! its parser, values, the evaluator with its built-in commands behind
//! the `System` trait, script options, prompt expansion, the job table
//! of job control and the line editor, and `program`, which gives them
//! the system on Mikros. Only `program` is left out of the host's build,
//! which runs the tests of the rest.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod ast;
mod edit;
mod eval;
mod jobs;
mod parse;
#[cfg(target_os = "mikros")]
mod program;
mod prompt;
mod value;

#[cfg(target_os = "mikros")]
fn main() {
    program::main();
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
