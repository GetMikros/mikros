//! Resolving types.

use super::{Checker, Scope};
use crate::ast::{self, Param, TypeRef};
use crate::model::*;
use crate::{Error, Span};

impl Checker<'_> {
    pub(super) fn ty(&self, scope: &Scope, t: &TypeRef) -> Result<Type, Error> {
        let span = &t.name.span;
        let params = &t.params;
        let opt = t.optional;
        let no_params = |ty: Type| {
            if !params.is_empty() {
                return Err(span.error(format!("`{}` takes no parameters", t.name.text())));
            }
            Ok(ty)
        };
        let plain = |ty: Type| {
            if opt {
                return Err(span.error(format!("`{}` cannot be optional", t.name.text())));
            }
            no_params(ty)
        };
        if let [name] = t.name.parts.as_slice() {
            if let Some(int) = Int::from_name(name) {
                return plain(Type::Int(int));
            }
            match name.as_str() {
                "bool" => return plain(Type::Bool),
                "f32" => return plain(Type::F32),
                "f64" => return plain(Type::F64),
                "string" => {
                    let [max] = params.as_slice() else {
                        return Err(span.error("`string` takes a maximum length"));
                    };
                    return Ok(Type::String {
                        max: self.count(scope, max, span)?,
                        optional: opt,
                    });
                }
                "vector" | "array" => {
                    let [Param::Type(element), count] = params.as_slice() else {
                        return Err(
                            span.error(format!("`{name}` takes an element type and a count"))
                        );
                    };
                    let element = Box::new(self.ty(scope, element)?);
                    let count = self.count(scope, count, span)?;
                    if name == "vector" {
                        return Ok(Type::Vector {
                            element,
                            max: count,
                            optional: opt,
                        });
                    }
                    if opt {
                        return Err(span.error("`array` cannot be optional"));
                    }
                    return Ok(Type::Array {
                        element,
                        len: count,
                    });
                }
                "handle" => return self.handle(params, span, opt),
                "client_end" | "server_end" => {
                    let [Param::Type(p)] = params.as_slice() else {
                        return Err(span.error(format!("`{name}` takes a protocol")));
                    };
                    let (full, decl, _) = self.lookup(scope, &p.name)?;
                    if !matches!(decl.kind, ast::DeclKind::Protocol(_))
                        || !p.params.is_empty()
                        || p.optional
                    {
                        return Err(p
                            .name
                            .span
                            .error(format!("`{}` is not a protocol", p.name.text())));
                    }
                    return Ok(Type::Endpoint {
                        client: name == "client_end",
                        protocol: full,
                        optional: opt,
                    });
                }
                _ => {}
            }
        }
        let (full, decl, decl_scope) = self.lookup(scope, &t.name)?;
        match &decl.kind {
            ast::DeclKind::Alias(target) => {
                if !params.is_empty() {
                    return Err(span.error("an alias takes no parameters"));
                }
                if !self.expanding.borrow_mut().insert(full.clone()) {
                    return Err(span.error(format!("alias `{full}` refers to itself")));
                }
                let resolved = self.ty(decl_scope, target);
                self.expanding.borrow_mut().remove(&full);
                let resolved = resolved?;
                if !opt {
                    return Ok(resolved);
                }
                make_optional(resolved)
                    .ok_or_else(|| span.error(format!("`{}` cannot be optional", t.name.text())))
            }
            ast::DeclKind::Struct(_) | ast::DeclKind::Union { .. } => no_params(Type::Named {
                name: full,
                optional: opt,
            }),
            ast::DeclKind::Enum { .. } | ast::DeclKind::Bits { .. } | ast::DeclKind::Table(_) => {
                plain(Type::Named {
                    name: full,
                    optional: false,
                })
            }
            _ => Err(span.error(format!("`{}` is not a type", t.name.text()))),
        }
    }

    fn handle(&self, params: &[Param], span: &Span, optional: bool) -> Result<Type, Error> {
        let name_of = |p: &Param| match p {
            Param::Type(t) if t.params.is_empty() && !t.optional && t.name.parts.len() == 1 => {
                Some(vec![t.name.clone()])
            }
            Param::Union(names) => Some(names.clone()),
            _ => None,
        };
        let object = match params.first() {
            None => None,
            Some(p) => {
                let names = name_of(p)
                    .filter(|n| n.len() == 1)
                    .ok_or_else(|| span.error("expected an object type"))?;
                let found = OBJECT_TYPES.iter().find(|(n, _)| *n == names[0].text());
                Some(
                    found
                        .ok_or_else(|| {
                            names[0]
                                .span
                                .error(format!("unknown object type `{}`", names[0].text()))
                        })?
                        .1,
                )
            }
        };
        let rights = match params.get(1) {
            None => None,
            Some(p) => {
                let names = name_of(p).ok_or_else(|| span.error("expected rights"))?;
                let mut rights = 0;
                for name in &names {
                    let found = RIGHTS.iter().find(|(n, _)| *n == name.text());
                    rights |= found
                        .ok_or_else(|| name.span.error(format!("unknown right `{}`", name.text())))?
                        .1;
                }
                Some(rights)
            }
        };
        if params.len() > 2 {
            return Err(span.error("`handle` takes an object type and rights"));
        }
        Ok(Type::Handle {
            object,
            rights,
            optional,
        })
    }
}

/// `ty` made optional, if its kind can be.
fn make_optional(ty: Type) -> Option<Type> {
    Some(match ty {
        Type::String { max, .. } => Type::String {
            max,
            optional: true,
        },
        Type::Vector { element, max, .. } => Type::Vector {
            element,
            max,
            optional: true,
        },
        Type::Handle { object, rights, .. } => Type::Handle {
            object,
            rights,
            optional: true,
        },
        Type::Endpoint {
            client, protocol, ..
        } => Type::Endpoint {
            client,
            protocol,
            optional: true,
        },
        Type::Named { name, .. } => Type::Named {
            name,
            optional: true,
        },
        _ => return None,
    })
}
