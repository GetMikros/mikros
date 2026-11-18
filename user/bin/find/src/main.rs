//! find, which walks folders and writes the paths of the entries for
//! which an expression is true, or acts on them. `primaries` holds what
//! find takes, splits its options from its expression, and answers
//! completion; `expression` reads the expression and decides it for an
//! entry, with the patterns of `pattern`; and `program`, left out of the
//! host's build, walks the folders, writes the paths and runs the
//! commands.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod expression;
mod pattern;
mod primaries;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
