//! Checks parsed MIDL files and builds their model: resolves names,
//! expands aliases, checks types, values and ordinals, and keeps only
//! what is available at the chosen version.

use crate::analysis::Reference;
use crate::ast;
use crate::model::*;
use crate::{Error, Span};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};

mod available;
mod contain;
mod limits;
mod types;
mod values;

use available::{Window, window};

/// Checks `files` and returns their libraries, keeping the elements
/// available at `version`, or all of them without one.
pub fn check(files: &[ast::File], version: Option<u64>) -> Result<Model, Error> {
    let mut checked = check_all(files, version);
    match checked.errors.is_empty() {
        true => Ok(checked.model),
        false => Err(checked.errors.remove(0)),
    }
}

/// What `check_all` found.
pub struct Checked {
    /// The libraries, without the declarations that have errors.
    pub model: Model,
    /// Every error, each once.
    pub errors: Vec<Error>,
    /// The names that resolved to declarations, each once, in the order
    /// they were resolved.
    pub references: Vec<Reference>,
}

/// Checks `files` like `check`, going on after errors. A file
/// whose library's `@available` is invalid adds no declarations. The
/// checks across declarations, for containment and message sizes, run
/// only when there is no other error.
pub fn check_all(files: &[ast::File], version: Option<u64>) -> Checked {
    let mut errors = Errors::default();
    let mut libraries: BTreeMap<String, Vec<&ast::File>> = BTreeMap::new();
    for file in files {
        let name = file.library.text();
        if !file.library.parts.iter().all(|p| {
            p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        }) {
            errors.push(
                file.library
                    .span
                    .error("library names are lowercase identifiers joined by dots"),
            );
        }
        libraries.entry(name).or_default().push(file);
    }
    let mut decls: HashMap<String, (&ast::Decl, Scope)> = HashMap::new();
    for (name, files) in &libraries {
        for file in files {
            let Some(library) = errors.take(window(&file.attrs, None, &file.library.span)) else {
                continue;
            };
            for using in &file.usings {
                if !libraries.contains_key(&using.text()) {
                    errors.push(
                        using
                            .span
                            .error(format!("unknown library `{}`", using.text())),
                    );
                }
            }
            let scope = Scope {
                library: name.clone(),
                usings: file.usings.iter().map(ast::Path::text).collect(),
                window: library,
            };
            for decl in &file.decls {
                if errors.take(scope.includes(&decl.attrs, &decl.span, version)) != Some(true) {
                    continue;
                }
                let full = format!("{name}.{}", decl.name);
                match decls.entry(full) {
                    Entry::Occupied(_) => errors.push(
                        decl.span
                            .error(format!("`{}` is declared twice", decl.name)),
                    ),
                    Entry::Vacant(slot) => {
                        slot.insert((decl, scope.clone()));
                    }
                }
            }
        }
    }
    let checker = Checker {
        decls: &decls,
        version,
        expanding: Default::default(),
        references: Default::default(),
    };
    let mut model = Model {
        libraries: Vec::new(),
    };
    let order = errors
        .take(order(&libraries))
        .unwrap_or_else(|| libraries.keys().cloned().collect());
    for name in order {
        let mut library = Library {
            name: name.clone(),
            doc: libraries[&name]
                .iter()
                .flat_map(|f| f.attrs.doc.clone())
                .collect(),
            decls: Vec::new(),
        };
        for file in &libraries[&name] {
            for decl in &file.decls {
                let full = format!("{name}.{}", decl.name);
                // A declaration declared twice is checked once.
                if let Some((found, scope)) = decls.get(&full)
                    && std::ptr::eq(*found, decl)
                    && let Some(kind) = errors.take(checker.decl(decl, scope))
                {
                    library.decls.push(Decl {
                        name: decl.name.clone(),
                        doc: decl.attrs.doc.clone(),
                        kind,
                    });
                }
            }
        }
        model.libraries.push(library);
    }
    if errors.0.is_empty() {
        errors.take(contain::no_self_containment(&model, &decls));
    }
    if errors.0.is_empty() {
        errors.take(limits::messages_fit(&model, &decls));
    }
    let mut seen = HashSet::new();
    let mut references = checker.references.into_inner();
    references.retain(|r| seen.insert((r.span.file.clone(), r.span.line, r.span.col)));
    Checked {
        model,
        errors: errors.0,
        references,
    }
}

/// The errors found so far, each once: a declaration with an error can
/// be checked again from the declarations that refer to it.
#[derive(Default)]
struct Errors(Vec<Error>);

impl Errors {
    fn push(&mut self, error: Error) {
        if !self.0.contains(&error) {
            self.0.push(error);
        }
    }

    fn take<T>(&mut self, result: Result<T, Error>) -> Option<T> {
        result.map_err(|e| self.push(e)).ok()
    }
}

/// Where a declaration's names are looked up, and the versions at which
/// its library exists.
#[derive(Clone)]
struct Scope {
    library: String,
    usings: Vec<String>,
    window: Window,
}

impl Scope {
    /// The full name of the library that `last`, the last identifier of
    /// a library name, refers to: the file's own library or one named by
    /// `using`. It is an error when more than one of them ends in `last`.
    ///
    /// TODO: counting the file's own library in that check is held until
    /// the clash between it and a used library is decided.
    fn short(&self, last: &str, span: &Span) -> Result<&str, Error> {
        let ends = |l: &&String| l.rsplit('.').next() == Some(last);
        let mut found = std::iter::once(&self.library)
            .chain(&self.usings)
            .filter(ends)
            .collect::<Vec<_>>();
        found.sort();
        found.dedup();
        match found.as_slice() {
            [library] => Ok(library),
            [] => Err(span.error(format!("library `{last}` is not named by `using`"))),
            _ => Err(span.error(format!(
                "`{last}` ends more than one library name; use the full library name"
            ))),
        }
    }
}

/// The libraries in an order in which each comes after those it uses.
fn order(libraries: &BTreeMap<String, Vec<&ast::File>>) -> Result<Vec<String>, Error> {
    fn visit(
        name: &str,
        libraries: &BTreeMap<String, Vec<&ast::File>>,
        state: &mut HashMap<String, bool>,
        out: &mut Vec<String>,
    ) -> Result<(), Error> {
        match state.get(name) {
            Some(true) => return Ok(()),
            Some(false) => {
                let span = &libraries[name][0].library.span;
                return Err(span.error(format!(
                    "libraries use each other in a cycle through `{name}`"
                )));
            }
            None => {}
        }
        state.insert(name.to_string(), false);
        for file in &libraries[name] {
            // An unknown library is an error of its own.
            for using in file
                .usings
                .iter()
                .filter(|u| libraries.contains_key(&u.text()))
            {
                visit(&using.text(), libraries, state, out)?;
            }
        }
        state.insert(name.to_string(), true);
        out.push(name.to_string());
        Ok(())
    }
    let (mut state, mut out) = (HashMap::new(), Vec::new());
    for name in libraries.keys() {
        visit(name, libraries, &mut state, &mut out)?;
    }
    Ok(out)
}

struct Checker<'a> {
    decls: &'a HashMap<String, (&'a ast::Decl, Scope)>,
    version: Option<u64>,
    /// The aliases and constants being resolved, to find cycles.
    expanding: std::cell::RefCell<HashSet<String>>,
    references: std::cell::RefCell<Vec<Reference>>,
}

impl Checker<'_> {
    /// The full name that `path` refers to from `scope`.
    fn lookup(
        &self,
        scope: &Scope,
        path: &ast::Path,
    ) -> Result<(String, &ast::Decl, &Scope), Error> {
        let full = match path.parts.as_slice() {
            [name] => format!("{}.{name}", scope.library),
            [library @ .., name] => {
                let library = library.join(".");
                if library == scope.library || scope.usings.contains(&library) {
                    format!("{library}.{name}")
                } else {
                    format!("{}.{name}", scope.short(&library, &path.span)?)
                }
            }
            [] => unreachable!("a path has a part"),
        };
        match self.decls.get(&full) {
            Some((decl, scope)) => {
                self.references.borrow_mut().push(Reference {
                    span: path.span.clone(),
                    end: path.end.clone(),
                    target: full.clone(),
                });
                Ok((full, decl, scope))
            }
            None => Err(path.span.error(format!(
                "`{}` is not declared or not available",
                path.text()
            ))),
        }
    }

    fn decl(&self, decl: &ast::Decl, scope: &Scope) -> Result<DeclKind, Error> {
        use ast::DeclKind as A;
        Ok(match &decl.kind {
            A::Const { .. } | A::Enum { .. } | A::Bits { .. } => self.value_decl(decl, scope)?,
            A::Alias(ty) => DeclKind::Alias(self.ty(scope, ty)?),
            A::Struct(members) => DeclKind::Struct(self.fields(scope, members)?),
            A::Union {
                strictness,
                members,
            } => DeclKind::Union {
                strictness: *strictness,
                members: self.ordinal_fields(scope, members, true)?,
            },
            A::Table(members) => DeclKind::Table(self.ordinal_fields(scope, members, false)?),
            A::Protocol(methods) => DeclKind::Protocol(self.methods(scope, methods)?),
        })
    }

    fn fields(&self, scope: &Scope, members: &[ast::Member]) -> Result<Vec<Field>, Error> {
        let mut out: Vec<Field> = Vec::new();
        for m in members {
            if !scope.includes(&m.attrs, &m.span, self.version)? {
                continue;
            }
            unique(&m.name, &m.span, out.iter().map(|o| &o.name))?;
            out.push(Field {
                name: m.name.clone(),
                doc: m.attrs.doc.clone(),
                ty: self.ty(scope, &m.ty)?,
            });
        }
        Ok(out)
    }

    fn ordinal_fields(
        &self,
        scope: &Scope,
        members: &[ast::OrdinalMember],
        union: bool,
    ) -> Result<Vec<OrdinalField>, Error> {
        let mut out: Vec<OrdinalField> = Vec::new();
        for m in members {
            if !scope.includes(&m.attrs, &m.span, self.version)? {
                continue;
            }
            unique(&m.name, &m.span, out.iter().map(|o| &o.name))?;
            if m.ordinal == 0 || out.iter().any(|o| o.ordinal == m.ordinal) {
                return Err(m.span.error(format!(
                    "ordinal {} must be unique and at least 1",
                    m.ordinal
                )));
            }
            let ty = self.ty(scope, &m.ty)?;
            if optional(&ty) {
                let what = if union { "union" } else { "table" };
                return Err(m.span.error(format!("{what} members cannot be optional")));
            }
            out.push(OrdinalField {
                ordinal: m.ordinal,
                name: m.name.clone(),
                doc: m.attrs.doc.clone(),
                ty,
            });
        }
        out.sort_by_key(|o| o.ordinal);
        Ok(out)
    }

    fn methods(&self, scope: &Scope, methods: &[ast::Method]) -> Result<Vec<Method>, Error> {
        let mut out: Vec<Method> = Vec::new();
        for m in methods {
            if !scope.includes(&m.attrs, &m.span, self.version)? {
                continue;
            }
            unique(&m.name, &m.span, out.iter().map(|o| &o.name))?;
            if !(1..1 << 31).contains(&m.ordinal) || out.iter().any(|o| o.ordinal == m.ordinal) {
                return Err(m.span.error(format!(
                    "ordinal {} must be unique and from 1 to 2^31 - 1",
                    m.ordinal
                )));
            }
            let kind = match &m.kind {
                ast::MethodKind::TwoWay {
                    request,
                    response,
                    error,
                } => {
                    let error = match error {
                        None => None,
                        Some(e) => {
                            let ty = self.ty(scope, e)?;
                            let fits = match &ty {
                                Type::Int(Int::I32) => true,
                                Type::Named {
                                    name,
                                    optional: false,
                                } => {
                                    matches!(self.decls[name].0.kind, ast::DeclKind::Enum { .. })
                                }
                                _ => false,
                            };
                            if !fits {
                                return Err(e
                                    .name
                                    .span
                                    .error("an error type must be an enumeration or i32"));
                            }
                            Some(ty)
                        }
                    };
                    MethodKind::TwoWay {
                        request: self.fields(scope, request)?,
                        response: self.fields(scope, response)?,
                        error,
                    }
                }
                ast::MethodKind::OneWay(args) => MethodKind::OneWay(self.fields(scope, args)?),
                ast::MethodKind::Event(args) => MethodKind::Event(self.fields(scope, args)?),
            };
            out.push(Method {
                name: m.name.clone(),
                doc: m.attrs.doc.clone(),
                ordinal: m.ordinal,
                strictness: m.strictness,
                kind,
            });
        }
        Ok(out)
    }
}

fn unique<'a>(
    name: &str,
    span: &Span,
    earlier: impl IntoIterator<Item = &'a String>,
) -> Result<(), Error> {
    if earlier.into_iter().any(|n| n == name) {
        return Err(span.error(format!("`{name}` is declared twice")));
    }
    Ok(())
}

fn optional(ty: &Type) -> bool {
    match ty {
        Type::String { optional, .. }
        | Type::Vector { optional, .. }
        | Type::Handle { optional, .. }
        | Type::Endpoint { optional, .. }
        | Type::Named { optional, .. } => *optional,
        _ => false,
    }
}
