//! msh, the Mikros shell, apart from what only its program on Mikros
//! does, so that it is tested on the host: the syntax tree of its
//! command language and its parser, values, the evaluator with its
//! built-in commands behind the `System` trait, script options, prompt
//! expansion, the job table of job control and the line editor.

pub mod ast;
pub mod edit;
pub mod eval;
pub mod jobs;
pub mod parse;
pub mod prompt;
pub mod value;
