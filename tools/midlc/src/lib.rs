//! `midlc`, the MIDL compiler: parses `.midl` files, checks them and
//! generates bindings.

pub mod ast;
pub mod check;
pub mod json;
pub mod layout;
pub mod lexer;
pub mod model;
pub mod parser;
pub mod rust;

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
    let (file, mut errors) = parse_all(file, text);
    match file {
        Some(file) if errors.is_empty() => Ok(file),
        _ => Err(errors.remove(0)),
    }
}

/// Parses the source `text` of the file named `file`, going on after
/// errors: the syntax tree without the declarations that have errors,
/// and every error. A lexical error ends the file, and the tree is
/// `None` when the file has no valid `library` declaration.
pub fn parse_all(file: &str, text: &str) -> (Option<ast::File>, Vec<Error>) {
    let tokens = match lexer::tokens(file, text) {
        Ok(tokens) => tokens,
        Err(e) => return (None, vec![e]),
    };
    let lines = text.lines().count().max(1);
    let end = Span {
        file: file.to_string(),
        line: lines,
        col: text.lines().last().map_or(1, |l| l.chars().count() + 1),
    };
    parser::parse_all(tokens, end)
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

/// Parses and checks the `(name, text)` source files like `compile`,
/// going on after errors: the libraries without the declarations that
/// have errors, and every error, those of each file in order.
pub fn compile_all(sources: &[(&str, &str)], version: Option<u64>) -> (model::Model, Vec<Error>) {
    let mut errors = Vec::new();
    let mut files = Vec::new();
    for (name, text) in sources {
        let (file, found) = parse_all(name, text);
        files.extend(file);
        errors.extend(found);
    }
    let (model, found) = check::check_all(&files, version);
    errors.extend(found);
    (model, errors)
}
