//! The parts of msh, the Mikros shell, that do not need the system, so
//! that they are tested on the host: the syntax tree of its command
//! language and the job table of job control.

pub mod ast;
pub mod jobs;
