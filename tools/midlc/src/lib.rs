//! `midlc`, the MIDL compiler: parses `.midl` files, checks them and
//! generates bindings.

pub mod ast;
pub mod check;
pub mod json;
pub mod layout;
pub mod lexer;
pub mod model;
pub mod parser;

#[cfg(test)]
mod tests;

/// Where something is in a source file.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn error(&self, message: impl Into<String>) -> Error {
        Error {
            span: self.clone(),
            message: message.into(),
        }
    }
}

/// A rejected file, with the place of the first problem.
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Span { file, line, col } = &self.span;
        write!(f, "{file}:{line}:{col}: {}", self.message)
    }
}

impl std::error::Error for Error {}

/// Parses the source `text` of the file named `file`.
pub fn parse(file: &str, text: &str) -> Result<ast::File, Error> {
    let tokens = lexer::tokens(file, text)?;
    let lines = text.lines().count().max(1);
    let end = Span {
        file: file.to_string(),
        line: lines,
        col: text.lines().last().map_or(1, |l| l.chars().count() + 1),
    };
    parser::parse(tokens, end)
}

/// Parses and checks the `(name, text)` source files, keeping what is
/// available at `version`, or everything without one.
pub fn compile(sources: &[(&str, &str)], version: Option<u64>) -> Result<model::Model, Error> {
    let files = sources
        .iter()
        .map(|(name, text)| parse(name, text))
        .collect::<Result<Vec<_>, _>>()?;
    check::check(&files, version)
}
