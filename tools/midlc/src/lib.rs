//! `midlc`, the MIDL compiler: parses `.midl` files, checks them and
//! generates bindings.

pub mod lexer;

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
