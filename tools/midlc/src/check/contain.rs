//! Finding structures that contain themselves.

use super::Scope;
use crate::Error;
use crate::ast;
use crate::model::*;
use std::collections::{HashMap, HashSet};

/// Rejects a structure that contains itself other than through an
/// optional member.
pub(super) fn no_self_containment(
    model: &Model,
    decls: &HashMap<String, (&ast::Decl, Scope)>,
) -> Result<(), Error> {
    fn inline_structs(ty: &Type, out: &mut Vec<String>) {
        match ty {
            Type::Named {
                name,
                optional: false,
            } => out.push(name.clone()),
            Type::Array { element, .. } => inline_structs(element, out),
            _ => {}
        }
    }
    fn visit(
        model: &Model,
        name: &str,
        path: &mut Vec<String>,
        done: &mut HashSet<String>,
    ) -> Option<String> {
        if path.iter().any(|p| p == name) {
            return Some(name.to_string());
        }
        if !done.insert(name.to_string()) {
            return None;
        }
        let Some(Decl {
            kind: DeclKind::Struct(fields),
            ..
        }) = model.decl(name)
        else {
            return None;
        };
        path.push(name.to_string());
        let mut inner = Vec::new();
        for f in fields {
            inline_structs(&f.ty, &mut inner);
        }
        let found = inner.iter().find_map(|n| visit(model, n, path, done));
        path.pop();
        found
    }
    for library in &model.libraries {
        for decl in &library.decls {
            let name = format!("{}.{}", library.name, decl.name);
            if let Some(cycle) = visit(model, &name, &mut Vec::new(), &mut HashSet::new()) {
                return Err(decls[&cycle].0.span.error(format!(
                    "`{cycle}` contains itself without an optional member"
                )));
            }
        }
    }
    Ok(())
}
