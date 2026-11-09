//! What editors need to know about MIDL sources: every error, the
//! declarations with where they are, and the names that refer to them.

use crate::ast::{self, Attributes, MethodKind};
use crate::model::Model;
use crate::{Error, Span, check, parse_all};

/// What `analyze` found in a set of source files.
pub struct Analysis {
    /// The libraries, without the declarations that have errors.
    pub model: Model,
    /// The errors, those of parsing each file first, in file order.
    pub diagnostics: Vec<Error>,
    /// The libraries, declarations and members that parsed, including
    /// those that failed to check.
    pub declarations: Vec<Declaration>,
    /// The names that resolved: library names in `using`, and the names
    /// of declarations.
    pub references: Vec<Reference>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Library,
    Const,
    Alias,
    Struct,
    Enum,
    Bits,
    Union,
    Table,
    Protocol,
    /// A member of a structure, enumeration, bit set, union or table.
    Member,
    Method,
    /// A method argument or result.
    Parameter,
}

/// A name being declared.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// The full name: the library name, then the declaration, member
    /// and parameter names, joined by dots. A library is declared in
    /// each of its files.
    pub name: String,
    pub kind: Kind,
    /// Where the declared identifier starts and ends.
    pub span: Span,
    pub end: Span,
    /// The documentation comment lines.
    pub doc: Vec<String>,
}

/// A name that refers to a declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// Where the name starts and ends.
    pub span: Span,
    pub end: Span,
    /// The full name of the library or declaration it refers to.
    pub target: String,
}

/// Parses and checks the `(name, text)` source files, going on after
/// errors, and indexes their names.
pub fn analyze(sources: &[(&str, &str)], version: Option<u64>) -> Analysis {
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();
    for (name, text) in sources {
        let (file, found) = parse_all(name, text);
        files.extend(file);
        diagnostics.extend(found);
    }
    let checked = check::check_all(&files, version);
    diagnostics.extend(checked.errors);
    let mut declarations = Vec::new();
    let mut references = Vec::new();
    for file in &files {
        let library = file.library.text();
        declarations.push(Declaration {
            name: library.clone(),
            kind: Kind::Library,
            span: file.library.span.clone(),
            end: file.library.end.clone(),
            doc: file.attrs.doc.clone(),
        });
        for using in &file.usings {
            if files.iter().any(|f| f.library.text() == using.text()) {
                references.push(Reference {
                    span: using.span.clone(),
                    end: using.end.clone(),
                    target: using.text(),
                });
            }
        }
        for decl in &file.decls {
            declare(&mut declarations, &library, decl);
        }
    }
    references.extend(checked.references);
    Analysis {
        model: checked.model,
        diagnostics,
        declarations,
        references,
    }
}

fn declare(out: &mut Vec<Declaration>, library: &str, decl: &ast::Decl) {
    use ast::DeclKind as A;
    let kind = match &decl.kind {
        A::Const { .. } => Kind::Const,
        A::Alias(_) => Kind::Alias,
        A::Struct(_) => Kind::Struct,
        A::Enum { .. } => Kind::Enum,
        A::Bits { .. } => Kind::Bits,
        A::Union { .. } => Kind::Union,
        A::Table(_) => Kind::Table,
        A::Protocol(_) => Kind::Protocol,
    };
    let full = format!("{library}.{}", decl.name);
    let mut add = |name: &str, kind, span: &Span, attrs: &Attributes| {
        out.push(Declaration {
            name: name.to_string(),
            kind,
            span: span.clone(),
            end: span.after(name.rsplit('.').next().unwrap_or(name)),
            doc: attrs.doc.clone(),
        });
    };
    add(&full, kind, &decl.span, &decl.attrs);
    let member = |name: &str| format!("{full}.{name}");
    match &decl.kind {
        A::Const { .. } | A::Alias(_) => {}
        A::Struct(members) => {
            for m in members {
                add(&member(&m.name), Kind::Member, &m.span, &m.attrs);
            }
        }
        A::Enum { members, .. } | A::Bits { members, .. } => {
            for m in members {
                add(&member(&m.name), Kind::Member, &m.span, &m.attrs);
            }
        }
        A::Union { members, .. } | A::Table(members) => {
            for m in members {
                add(&member(&m.name), Kind::Member, &m.span, &m.attrs);
            }
        }
        A::Protocol(methods) => {
            for method in methods {
                let name = member(&method.name);
                add(&name, Kind::Method, &method.span, &method.attrs);
                let params: Vec<&ast::Member> = match &method.kind {
                    MethodKind::TwoWay {
                        request, response, ..
                    } => request.iter().chain(response).collect(),
                    MethodKind::OneWay(p) | MethodKind::Event(p) => p.iter().collect(),
                };
                for p in params {
                    let param = format!("{name}.{}", p.name);
                    add(&param, Kind::Parameter, &p.span, &p.attrs);
                }
            }
        }
    }
}
